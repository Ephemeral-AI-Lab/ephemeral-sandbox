"""Workload helpers for the live_e2e_test occ/ suite.

The OCC bookkeeping layer is host-side per the migration plan, but the
gitignore oracle and the layer-stack base view it commits against must
reflect what an agent would actually see inside the sandbox. These
helpers bridge both:

- ``write_sandbox_gitignore`` writes ``.gitignore`` into ``/testbed`` via
  ``raw_exec`` (the same path agents use), so route decisions consult
  the real sandbox state.
- ``make_sandbox_gitignore_run_fn`` returns a ``RunFn`` that ships
  ``git check-ignore`` over ``raw_exec`` so the host-side oracle's
  decisions are computed against ``/testbed``, not a synthetic host
  workspace.
- ``seed_sandbox_file`` writes a payload to ``/testbed`` AND publishes
  it into the host-side layer-stack base view so the OCC manifest
  mirrors the sandbox.

Plus latency aggregation utilities used by the per-path-CAS load loop.
"""

from __future__ import annotations

import asyncio
import base64
import json
import shlex
import statistics
import subprocess
import time
from collections.abc import Sequence
from dataclasses import dataclass, field
from pathlib import Path

from sandbox.api.tool.raw_exec import raw_exec as raw_exec_fn
from sandbox.layer_stack.changes import LayerChange
from sandbox.layer_stack.stack_manager import LayerStackManager
from sandbox.occ.content.gitignore_oracle import RunFn, RunOutcome
from sandbox.occ.content.hashing import ContentHasher


# -- Workspace seeding ---------------------------------------------------


async def write_sandbox_gitignore(
    sandbox_id: str, patterns: Sequence[str]
) -> None:
    """Replace ``/testbed/.gitignore`` and commit it inside the sandbox.

    The OCC ``GitignoreOracle`` is bridged to ``/testbed`` (see
    ``make_sandbox_gitignore_run_fn``); writing patterns through this
    helper is what test bodies rely on to flip route decisions between
    tracked and direct.
    """
    body = "\n".join(patterns) + ("\n" if patterns else "")
    encoded = base64.b64encode(body.encode("utf-8")).decode("ascii")
    cmd = (
        "set -e; cd /testbed; "
        f"printf '%s' {shlex.quote(encoded)} | base64 -d > .gitignore; "
        "git -c user.email=eos@local -c user.name=eos add .gitignore; "
        "git -c user.email=eos@local -c user.name=eos "
        "    commit -q --allow-empty -m 'live-e2e-occ:gitignore'"
    )
    result = await raw_exec_fn(sandbox_id, cmd, timeout=30)
    if result.exit_code != 0:
        raise RuntimeError(
            f"write_sandbox_gitignore failed: rc={result.exit_code} "
            f"stderr={result.stderr!r}"
        )


def make_sandbox_gitignore_run_fn(
    sandbox_id: str, loop: asyncio.AbstractEventLoop
) -> RunFn:
    """Build a sync :class:`RunFn` that bridges ``git check-ignore`` to
    the live sandbox via ``raw_exec``.

    ``GitignoreOracle.is_ignored`` runs synchronously inside the OCC
    orchestrator's worker thread (``OccService.prepare_changeset_sync``
    is offloaded via ``run_sync_in_executor``). To call the async
    ``raw_exec_fn`` from that thread we schedule the coroutine on the
    test's running loop via :func:`asyncio.run_coroutine_threadsafe`.

    The returned function ignores the host-side ``-C <path>`` token in
    ``argv`` and replaces the workspace with ``/testbed``; everything
    else (``check-ignore``, ``-z``, ``--stdin``, ``--verbose``,
    ``--non-matching``, plus any flags the oracle adds) ships verbatim.
    """

    def _run(argv: list[str], stdin_bytes: bytes) -> RunOutcome:
        # argv shape: ["git", "-C", <workspace>, "check-ignore", "-z", ...]
        # Drop the host-side workspace and target /testbed instead.
        if len(argv) < 4 or argv[0] != "git" or argv[1] != "-C":
            raise RuntimeError(f"unexpected gitignore argv: {argv!r}")
        rebuilt = ["git", "-C", "/testbed", *argv[3:]]
        encoded = base64.b64encode(stdin_bytes).decode("ascii")
        # `printf %s | base64 -d` keeps NUL-delimited stdin intact end-to-end.
        cmd = (
            f"printf '%s' {shlex.quote(encoded)} | base64 -d | "
            + " ".join(shlex.quote(token) for token in rebuilt)
        )
        future = asyncio.run_coroutine_threadsafe(
            raw_exec_fn(sandbox_id, cmd, timeout=30),
            loop,
        )
        result = future.result(timeout=35)
        return RunOutcome(
            returncode=int(result.exit_code),
            stdout=(
                result.stdout.encode("utf-8")
                if isinstance(result.stdout, str)
                else (result.stdout or b"")
            ),
            stderr=(
                result.stderr.encode("utf-8")
                if isinstance(result.stderr, str)
                else (result.stderr or b"")
            ),
        )

    return _run


async def seed_sandbox_file(
    sandbox_id: str,
    manager: LayerStackManager,
    payloads_dir: Path,
    rel_path: str,
    content: bytes,
) -> str:
    """Seed *both* ``/testbed`` and the host-side layer stack with *content*.

    Writes the file inside the sandbox via ``raw_exec`` (so a real
    ``read_file`` would see it) and publishes the matching layer change
    on the host-side :class:`LayerStackManager` so the OCC view stays in
    sync. Returns the content hash for callers that need to pin a base
    hash.
    """
    encoded = base64.b64encode(content).decode("ascii")
    target = f"/testbed/{rel_path}"
    cmd = (
        "set -e; "
        f"mkdir -p {shlex.quote(str(Path(target).parent))}; "
        f"printf '%s' {shlex.quote(encoded)} | base64 -d > {shlex.quote(target)}"
    )
    result = await raw_exec_fn(sandbox_id, cmd, timeout=30)
    if result.exit_code != 0:
        raise RuntimeError(
            f"seed_sandbox_file({rel_path}) failed: rc={result.exit_code} "
            f"stderr={result.stderr!r}"
        )
    return publish_base_file(manager, payloads_dir, rel_path, content)


async def read_sandbox_file(sandbox_id: str, rel_path: str) -> tuple[bytes, bool]:
    """Read a file from ``/testbed`` for cross-consistency checks.

    Returns ``(content, exists)``. Used by tests that want to verify
    OCC's host-side manifest changes correspond to actual on-sandbox
    state (e.g., the seeded base view did land, the content matches).
    """
    target = f"/testbed/{rel_path}"
    cmd = (
        f"if [ -f {shlex.quote(target)} ]; then "
        f"  base64 < {shlex.quote(target)}; "
        "else echo MISSING; fi"
    )
    result = await raw_exec_fn(sandbox_id, cmd, timeout=30)
    if result.exit_code != 0:
        raise RuntimeError(
            f"read_sandbox_file({rel_path}) failed: rc={result.exit_code} "
            f"stderr={result.stderr!r}"
        )
    out = (result.stdout or "").strip()
    if out == "MISSING":
        return b"", False
    return base64.b64decode(out), True


def init_git_workspace(workspace: Path) -> None:
    """Initialize a host-side git workspace so ``GitignoreOracle`` works.

    The OCC orchestrator routes via ``git check-ignore`` against this
    workspace; without a real ``.git`` directory the oracle errors out.
    """
    workspace.mkdir(parents=True, exist_ok=True)
    subprocess.run(
        ["git", "-C", str(workspace), "init", "-q", "-b", "main"],
        check=True,
    )
    subprocess.run(
        [
            "git",
            "-C",
            str(workspace),
            "-c",
            "user.email=eos@local",
            "-c",
            "user.name=eos",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "live-e2e-occ:baseline",
        ],
        check=True,
    )


def write_gitignore(workspace: Path, patterns: Sequence[str]) -> None:
    """Write ``.gitignore`` into *workspace* and stage+commit it.

    The oracle reads patterns from the working tree, so a write is
    sufficient — but committing keeps the workspace self-consistent if
    callers ever ``git ls-files`` it later.
    """
    (workspace / ".gitignore").write_text(
        "\n".join(patterns) + ("\n" if patterns else ""),
        encoding="utf-8",
    )
    subprocess.run(
        ["git", "-C", str(workspace), "add", ".gitignore"],
        check=True,
    )
    subprocess.run(
        [
            "git",
            "-C",
            str(workspace),
            "-c",
            "user.email=eos@local",
            "-c",
            "user.name=eos",
            "commit",
            "-q",
            "-m",
            "gitignore",
        ],
        check=True,
    )


def publish_base_file(
    manager: LayerStackManager,
    payloads_dir: Path,
    rel_path: str,
    content: bytes,
) -> str:
    """Seed the layer-stack base view with one file.

    Returns the content hash so callers can pin a base hash for CAS tests.
    """
    payloads_dir.mkdir(parents=True, exist_ok=True)
    safe_name = rel_path.replace("/", "_")
    payload = payloads_dir / f"base-{safe_name}-{time.perf_counter_ns()}"
    payload.write_bytes(content)
    digest = ContentHasher().hash_bytes(content)
    manager.publish_changes(
        [
            LayerChange(
                path=rel_path,
                kind="write",
                content_hash=digest,
                source_path=str(payload),
            )
        ]
    )
    return digest


# -- Latency / metric collection -----------------------------------------


@dataclass
class IterationStat:
    test: str
    accepted: int
    rejected: int
    latency_ms: float
    manifest_version: int | None
    manifest_lag: int | None
    ts: float = field(default_factory=time.time)


@dataclass
class LoadCollector:
    """Session-scoped accumulator for occ load-loop iterations."""

    output_path: Path
    stats: list[IterationStat] = field(default_factory=list)

    def record(self, stat: IterationStat) -> None:
        self.stats.append(stat)
        self.output_path.parent.mkdir(parents=True, exist_ok=True)
        with self.output_path.open("a", encoding="utf-8") as handle:
            handle.write(json.dumps(stat.__dict__, sort_keys=True) + "\n")

    def summarize(self) -> list[dict[str, float | int | str]]:
        by_test: dict[str, list[IterationStat]] = {}
        for stat in self.stats:
            by_test.setdefault(stat.test, []).append(stat)
        rows: list[dict[str, float | int | str]] = []
        for name, entries in sorted(by_test.items()):
            latencies = [entry.latency_ms for entry in entries]
            rows.append(
                {
                    "test": name,
                    "iters": len(entries),
                    "accepted": sum(entry.accepted for entry in entries),
                    "rejected": sum(entry.rejected for entry in entries),
                    "p50_ms": _percentile(latencies, 0.50),
                    "p99_ms": _percentile(latencies, 0.99),
                    "mean_ms": statistics.fmean(latencies) if latencies else 0.0,
                    "max_ms": max(latencies) if latencies else 0.0,
                }
            )
        return rows


def _percentile(samples: list[float], pct: float) -> float:
    if not samples:
        return 0.0
    ordered = sorted(samples)
    index = max(0, min(len(ordered) - 1, int(round((len(ordered) - 1) * pct))))
    return ordered[index]


def render_table(rows: list[dict[str, float | int | str]]) -> str:
    if not rows:
        return "(no load metrics recorded)"
    headers = ["test", "iters", "accepted", "rejected", "p50_ms", "p99_ms", "mean_ms", "max_ms"]
    formatted: list[list[str]] = [headers]
    for row in rows:
        formatted.append(
            [
                str(row["test"]),
                str(row["iters"]),
                str(row["accepted"]),
                str(row["rejected"]),
                f"{float(row['p50_ms']):.2f}",
                f"{float(row['p99_ms']):.2f}",
                f"{float(row['mean_ms']):.2f}",
                f"{float(row['max_ms']):.2f}",
            ]
        )
    widths = [max(len(cell) for cell in column) for column in zip(*formatted)]
    lines = []
    for row_index, row in enumerate(formatted):
        line = " | ".join(cell.ljust(width) for cell, width in zip(row, widths))
        lines.append(line)
        if row_index == 0:
            lines.append("-+-".join("-" * width for width in widths))
    return "\n".join(lines)


__all__ = [
    "IterationStat",
    "LoadCollector",
    "init_git_workspace",
    "make_sandbox_gitignore_run_fn",
    "publish_base_file",
    "read_sandbox_file",
    "render_table",
    "seed_sandbox_file",
    "write_gitignore",
    "write_sandbox_gitignore",
]
