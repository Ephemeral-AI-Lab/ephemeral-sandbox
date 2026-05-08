"""Progressive live-test tier runner.

Implements §7 of progressive-live-test-tiers-design-20260508.md.

Public entry point: ``python -m backend.tests.live_e2e_test._tools.run_tiered``.

Design notes (advisor-flagged):

* Budget enforcement uses ``subprocess.Popen`` + ``start_new_session=True``
  rather than ``asyncio.wait_for`` — pytest is a child process and we
  need to deliver SIGINT then SIGKILL to its process group when the
  per-tier wall budget is exceeded.
* Artifact path stability is handed to child tests via the
  ``EOS_TIER_RUN_ID`` environment variable. Existing tests honour the
  variable when set and fall back to their old ISO+pid filenames when
  invoked standalone, so backwards-compatibility is preserved.
* Tier 0 is a Python-side health probe (no fork). Every other tier
  shells out to pytest under a process-group timer.
"""

from __future__ import annotations

import argparse
import datetime as _dt
import json
import os
import signal
import subprocess
import sys
import time
import tomllib
from collections.abc import Callable
from dataclasses import dataclass, field
from pathlib import Path
from typing import Literal

# Import sibling probe via relative path so the module loads from either
# `python -m backend.tests.live_e2e_test._tools.run_tiered` or as a script.
try:
    from .tier0_health import Tier0Result, probe_tier0
except ImportError:  # pragma: no cover — fallback for direct script invocation
    from tier0_health import Tier0Result, probe_tier0  # type: ignore[no-redef]


# --------------------------------------------------------------------------
# Configuration model
# --------------------------------------------------------------------------


CascadeKind = Literal["abort_all", "abort_ge", "abort_eq", "warn", "none"]
TierKind = Literal["tier0_health", "pytest"]
TierStatus = Literal[
    "passed", "failed", "aborted_budget", "skipped_cascade", "skipped_unavailable"
]


@dataclass(frozen=True)
class TierConfig:
    id: int
    name: str
    wall_budget_s: float
    kind: TierKind
    cascade: CascadeKind
    pytest_args: list[str] = field(default_factory=list)
    per_cell_budget_s: float | None = None
    cascade_target: int | None = None
    api_url: str = "http://localhost:3000/api"


@dataclass
class TierOutcome:
    tier_id: int
    name: str
    status: TierStatus
    elapsed_s: float
    artifact_path: str | None = None
    failed_cells: int | None = None
    notes: str = ""


# --------------------------------------------------------------------------
# TOML loading
# --------------------------------------------------------------------------


_VALID_CASCADE: set[CascadeKind] = {"abort_all", "abort_ge", "abort_eq", "warn", "none"}
_VALID_KIND: set[TierKind] = {"tier0_health", "pytest"}


def load_tier_configs(path: Path) -> list[TierConfig]:
    with path.open("rb") as fh:
        data = tomllib.load(fh)
    raw_tiers = data.get("tier", [])
    if not isinstance(raw_tiers, list) or not raw_tiers:
        raise ValueError(f"{path}: missing [[tier]] entries")

    tiers: list[TierConfig] = []
    seen_ids: set[int] = set()
    for entry in raw_tiers:
        kind = entry.get("kind")
        cascade = entry.get("cascade")
        if kind not in _VALID_KIND:
            raise ValueError(f"tier {entry.get('id')!r}: invalid kind={kind!r}")
        if cascade not in _VALID_CASCADE:
            raise ValueError(f"tier {entry.get('id')!r}: invalid cascade={cascade!r}")
        if cascade in ("abort_ge", "abort_eq") and entry.get("cascade_target") is None:
            raise ValueError(
                f"tier {entry.get('id')!r}: cascade={cascade} requires cascade_target"
            )
        tier_id = int(entry["id"])
        if tier_id in seen_ids:
            raise ValueError(f"duplicate tier id {tier_id}")
        seen_ids.add(tier_id)
        tiers.append(
            TierConfig(
                id=tier_id,
                name=str(entry["name"]),
                wall_budget_s=float(entry["wall_budget_s"]),
                kind=kind,
                cascade=cascade,
                pytest_args=list(entry.get("pytest_args", [])),
                per_cell_budget_s=(
                    float(entry["per_cell_budget_s"])
                    if entry.get("per_cell_budget_s") is not None
                    else None
                ),
                cascade_target=(
                    int(entry["cascade_target"])
                    if entry.get("cascade_target") is not None
                    else None
                ),
                api_url=str(entry.get("api_url", "http://localhost:3000/api")),
            )
        )
    tiers.sort(key=lambda t: t.id)
    return tiers


# --------------------------------------------------------------------------
# Cascade rules — pure logic, unit-testable
# --------------------------------------------------------------------------


@dataclass
class CascadeState:
    """Tracks which tier ids should be skipped based on prior failures.

    ``skip_threshold``: tiers with id >= threshold are skipped (set by
    ``abort_ge``, e.g. "abort 2+" in plan §3).
    ``skip_ids``: single tier ids skipped (set by ``abort_eq``, e.g.
    "abort 5", "abort 6").
    """

    abort_all: bool = False
    skip_threshold: int | None = None
    skip_ids: set[int] = field(default_factory=set)

    def record(self, tier: TierConfig, status: TierStatus) -> None:
        if status == "passed":
            return
        if status in ("skipped_cascade", "skipped_unavailable"):
            return
        if tier.cascade == "abort_all":
            self.abort_all = True
        elif tier.cascade == "abort_ge":
            target = tier.cascade_target
            if target is not None:
                if self.skip_threshold is None or target < self.skip_threshold:
                    self.skip_threshold = target
        elif tier.cascade == "abort_eq":
            target = tier.cascade_target
            if target is not None:
                self.skip_ids.add(target)
        # warn / none → no cascade effect

    def should_skip(self, tier: TierConfig) -> bool:
        if self.abort_all:
            return True
        if self.skip_threshold is not None and tier.id >= self.skip_threshold:
            return True
        if tier.id in self.skip_ids:
            return True
        return False


# --------------------------------------------------------------------------
# Subprocess helpers
# --------------------------------------------------------------------------


@dataclass
class SubprocessOutcome:
    returncode: int
    timed_out: bool
    stdout_tail: str
    stderr_tail: str
    elapsed_s: float


def _terminate_group(pid: int, sig: int) -> None:
    try:
        pgid = os.getpgid(pid)
    except (ProcessLookupError, OSError):
        return
    try:
        os.killpg(pgid, sig)
    except (ProcessLookupError, OSError):
        return


def run_with_budget(
    argv: list[str],
    *,
    env: dict[str, str],
    wall_budget_s: float,
    grace_s: float = 30.0,
    cwd: str | Path | None = None,
    popen_factory: Callable[..., subprocess.Popen[bytes]] | None = None,
    clock: Callable[[], float] = time.monotonic,
) -> SubprocessOutcome:
    """Run ``argv`` under a wall-clock budget with SIGINT-then-SIGKILL.

    The child is spawned with ``start_new_session=True`` so we can signal
    the whole process group on timeout. Stdout/stderr are streamed to
    temp files (NOT ``subprocess.PIPE``) so a verbose pytest run can't
    fill the OS pipe buffer and deadlock the runner — pipe buffers are
    fixed-size (~16-64 KB on macOS) and Popen.communicate() only drains
    them on timeout/exit. ``popen_factory`` is the seam unit tests
    inject a fake popen through; the fake's ``communicate`` is used
    when the factory does not produce a real Popen with a temp-file
    backed stdout.
    """
    import tempfile

    factory = popen_factory or subprocess.Popen
    started = clock()
    using_tempfiles = factory is subprocess.Popen
    if using_tempfiles:
        stdout_f = tempfile.TemporaryFile(mode="w+b")
        stderr_f = tempfile.TemporaryFile(mode="w+b")
        proc = factory(
            argv,
            env=env,
            cwd=cwd,
            stdin=subprocess.DEVNULL,
            stdout=stdout_f,
            stderr=stderr_f,
            start_new_session=True,
        )
    else:
        # Fake Popen path used by unit tests — keep PIPE semantics so
        # the fake's communicate() can return canned bytes.
        stdout_f = None
        stderr_f = None
        proc = factory(
            argv,
            env=env,
            cwd=cwd,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            start_new_session=True,
        )

    def _read_tails() -> tuple[str, str]:
        if stdout_f is None or stderr_f is None:
            return "", ""
        stdout_f.seek(0)
        stderr_f.seek(0)
        out = _tail(stdout_f.read())
        err = _tail(stderr_f.read())
        stdout_f.close()
        stderr_f.close()
        return out, err

    timed_out = False
    try:
        if using_tempfiles:
            proc.wait(timeout=wall_budget_s)
            stdout = b""
            stderr = b""
        else:
            stdout, stderr = proc.communicate(timeout=wall_budget_s)
    except subprocess.TimeoutExpired:
        timed_out = True
        _terminate_group(proc.pid, signal.SIGINT)
        try:
            if using_tempfiles:
                proc.wait(timeout=grace_s)
            else:
                stdout, stderr = proc.communicate(timeout=grace_s)
        except subprocess.TimeoutExpired:
            _terminate_group(proc.pid, signal.SIGKILL)
            if using_tempfiles:
                proc.wait()
            else:
                stdout, stderr = proc.communicate()

    if using_tempfiles:
        stdout_tail, stderr_tail = _read_tails()
    else:
        stdout_tail = _tail(stdout)
        stderr_tail = _tail(stderr)

    elapsed = clock() - started
    return SubprocessOutcome(
        returncode=(
            proc.returncode
            if proc.returncode is not None
            else (-signal.SIGKILL if timed_out else -1)
        ),
        timed_out=timed_out,
        stdout_tail=stdout_tail,
        stderr_tail=stderr_tail,
        elapsed_s=elapsed,
    )


def _tail(blob: bytes | None, *, limit: int = 1024) -> str:
    if not blob:
        return ""
    text = blob.decode("utf-8", errors="replace")
    if len(text) <= limit:
        return text
    return "..." + text[-limit:]


# --------------------------------------------------------------------------
# Tier execution
# --------------------------------------------------------------------------


def execute_tier(
    tier: TierConfig,
    *,
    run_id: str,
    project_root: Path,
    results_dir: Path,
    tier0_probe: Callable[[str], Tier0Result] | None = None,
    subprocess_runner: Callable[..., SubprocessOutcome] | None = None,
    clock: Callable[[], float] = time.monotonic,
) -> TierOutcome:
    """Run one tier and return its outcome.

    ``tier0_probe`` and ``subprocess_runner`` are injection seams so unit
    tests can drive the runner without forking real processes.
    """
    runner = subprocess_runner or run_with_budget

    if tier.kind == "tier0_health":
        probe = tier0_probe or (lambda url: probe_tier0(url, timeout_s=5.0))
        start = clock()
        result = probe(tier.api_url)
        elapsed = clock() - start
        return TierOutcome(
            tier_id=tier.id,
            name=tier.name,
            status="passed" if result.passed else "failed",
            elapsed_s=elapsed,
            failed_cells=0 if result.passed else 1,
            notes=result.notes,
        )

    # pytest tier
    env = os.environ.copy()
    env["EOS_TIER_RUN_ID"] = run_id
    env["EOS_TIER_ID"] = str(tier.id)
    pytest_argv = [sys.executable, "-m", "pytest", "-q", *tier.pytest_args]
    outcome = runner(
        pytest_argv,
        env=env,
        wall_budget_s=tier.wall_budget_s,
        cwd=project_root,
        clock=clock,
    )
    if outcome.timed_out:
        status: TierStatus = "aborted_budget"
        notes = f"wall_budget_s={tier.wall_budget_s} exceeded; SIGKILL after grace"
    elif outcome.returncode == 0:
        status = "passed"
        notes = ""
    else:
        status = "failed"
        notes = (
            f"pytest_returncode={outcome.returncode}; "
            f"stderr_tail={outcome.stderr_tail[-300:]!r}"
        )

    failed_cells = _count_failed_cells(results_dir, tier.id, run_id)
    return TierOutcome(
        tier_id=tier.id,
        name=tier.name,
        status=status,
        elapsed_s=outcome.elapsed_s,
        failed_cells=failed_cells,
        notes=notes,
    )


def _count_failed_cells(results_dir: Path, tier_id: int, run_id: str) -> int | None:
    """Best-effort: count failed_cells across this tier's artifacts.

    Reads any ``*-{run_id}.jsonl`` under ``results_dir`` and tallies rows
    whose ``passed`` is explicitly False. Returns None if no matching
    artifacts exist, so the caller can disambiguate "no data" from "0".
    """
    if not results_dir.exists():
        return None
    found_any = False
    failed = 0
    for artifact in sorted(results_dir.glob(f"*-{run_id}.jsonl")):
        found_any = True
        try:
            with artifact.open(encoding="utf-8") as fh:
                for line in fh:
                    try:
                        row = json.loads(line)
                    except json.JSONDecodeError:
                        continue
                    if row.get("passed") is False:
                        failed += 1
        except OSError:
            continue
    return failed if found_any else None


# --------------------------------------------------------------------------
# Aggregation + driver
# --------------------------------------------------------------------------


@dataclass
class RunSummary:
    run_id: str
    outcomes: list[TierOutcome]
    summary_path: Path

    @property
    def exit_code(self) -> int:
        for outcome in self.outcomes:
            if outcome.status in ("failed", "aborted_budget"):
                return 1
        return 0


def write_summary(outcomes: list[TierOutcome], path: Path, run_id: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", encoding="utf-8") as fh:
        for outcome in outcomes:
            row = {
                "schema": "progressive_test.tier_summary.v1",
                "tier": outcome.tier_id,
                "name": outcome.name,
                "status": outcome.status,
                "elapsed_s": round(outcome.elapsed_s, 3),
                "artifact_path": outcome.artifact_path,
                "failed_cells": outcome.failed_cells,
                "notes": outcome.notes,
                "run_id": run_id,
            }
            fh.write(json.dumps(row, sort_keys=True, separators=(",", ":")))
            fh.write("\n")


def run(
    tiers: list[TierConfig],
    *,
    project_root: Path,
    results_dir: Path,
    run_id: str | None = None,
    tier_filter: list[int] | None = None,
    no_cascade: bool = False,
    tier0_probe: Callable[[str], Tier0Result] | None = None,
    subprocess_runner: Callable[..., SubprocessOutcome] | None = None,
    clock: Callable[[], float] = time.monotonic,
) -> RunSummary:
    if run_id is None:
        run_id = _dt.datetime.now(_dt.timezone.utc).strftime(
            "%Y%m%dT%H%M%SZ"
        ) + f"-{os.getpid()}"

    selected = (
        tiers if tier_filter is None else [t for t in tiers if t.id in set(tier_filter)]
    )
    cascade = CascadeState()
    outcomes: list[TierOutcome] = []
    for tier in selected:
        if not no_cascade and cascade.should_skip(tier):
            outcomes.append(
                TierOutcome(
                    tier_id=tier.id,
                    name=tier.name,
                    status="skipped_cascade",
                    elapsed_s=0.0,
                    notes="skipped due to cascade rule",
                )
            )
            continue
        outcome = execute_tier(
            tier,
            run_id=run_id,
            project_root=project_root,
            results_dir=results_dir,
            tier0_probe=tier0_probe,
            subprocess_runner=subprocess_runner,
            clock=clock,
        )
        outcomes.append(outcome)
        cascade.record(tier, outcome.status)

    summary_path = results_dir / f"progressive-test-summary-{run_id}.jsonl"
    write_summary(outcomes, summary_path, run_id)
    return RunSummary(run_id=run_id, outcomes=outcomes, summary_path=summary_path)


def _parse_tier_filter(value: str | None) -> list[int] | None:
    if not value:
        return None
    parts = [p.strip() for p in value.split(",") if p.strip()]
    return [int(p) for p in parts]


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Progressive live-test tier runner")
    parser.add_argument(
        "--tier", default=None, help="Comma-separated tier ids (default: all)"
    )
    parser.add_argument("--run-id", default=None, help="Override the run identifier")
    parser.add_argument(
        "--no-cascade",
        action="store_true",
        help="Run every selected tier ignoring cascade rules (for debugging).",
    )
    parser.add_argument(
        "--config",
        default=None,
        help="Path to tiers.toml (default: alongside this script)",
    )
    args = parser.parse_args(argv)

    here = Path(__file__).resolve().parent
    config = Path(args.config) if args.config else here / "tiers.toml"
    tiers = load_tier_configs(config)

    project_root = here.parent.parent.parent.parent  # ~/.../EphemeralOS
    results_dir = project_root / ".omc" / "results"

    summary = run(
        tiers,
        project_root=project_root,
        results_dir=results_dir,
        run_id=args.run_id,
        tier_filter=_parse_tier_filter(args.tier),
        no_cascade=args.no_cascade,
    )

    print(f"\n[run_tiered] summary={summary.summary_path}")
    print(f"[run_tiered] run_id={summary.run_id}")
    for outcome in summary.outcomes:
        print(
            f"  tier {outcome.tier_id:>1} [{outcome.name:<24}] "
            f"{outcome.status:<18} elapsed={outcome.elapsed_s:>7.2f}s "
            f"failed_cells={outcome.failed_cells} {outcome.notes}"
        )
    return summary.exit_code


if __name__ == "__main__":
    sys.exit(main())
