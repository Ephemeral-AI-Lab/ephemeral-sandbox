"""Direct-RPC probe for ``shell(background=True)`` integration tests.

Phase 2 plan §Step 3. Rather than going through the mock-agent + scenario
machinery (which would require routing ``tools.background.*`` through the
mock-agent harness — out of scope for this phase), the probe issues
:mod:`sandbox.api` calls directly with a pre-built ``SandboxCaller``. The
live tests (T1 – T8) drive the probe with a sandbox_id obtained from
``sweevo_image_instance`` and assert on the returned summary plus
``sandbox_events.jsonl`` invariants.

Three modes share the seed + reconcile shape used by
``heavy_io_zoned_probe``:

- ``golden`` — N concurrent background launches; wait for natural exit.
- ``cancel`` — N launches of long-running ``sleep`` commands; cancel each
  after ``cancel_after_s``; assert no leaked upperdir on the next foreground
  read.
- ``interleave`` — 1 long-running background launch + M short foreground
  shells interleaved; record per-foreground mount-latency timings for
  AC-3 (foreground p95 mount latency unchanged).
"""

from __future__ import annotations

import asyncio
import json
import time
from collections.abc import Iterable
from dataclasses import dataclass, field

import sandbox.api as sandbox_api
from sandbox._shared.models import (
    SandboxCaller,
    ShellRequest,
    ShellResult,
)


WORKSPACE_ROOT = "/testbed"
ROOT = f"{WORKSPACE_ROOT}/.ephemeralos/sweevo-mock/background_shell"
SUMMARY_PATH = f"{ROOT}/summary.json"
SUMMARY_SCHEMA = "task_center_runner.background_shell.v1"


# Long-running sleep used by all three modes. The cancel/interleave modes
# need a window large enough that they can race the daemon's TTL reaper +
# foreground operations.
DEFAULT_BACKGROUND_SLEEP_S = 30
DEFAULT_INTERLEAVE_COUNT = 5
DEFAULT_CANCEL_AFTER_S = 1.0
DEFAULT_TIMEOUT_S = 120


@dataclass
class _LaunchRecord:
    """One background launch's lifecycle observation."""

    index: int
    started_at: float
    completed_at: float | None = None
    status: str = ""
    exit_code: int | None = None
    changed_paths_count: int = 0
    cancelled: bool = False
    error: str | None = None


@dataclass
class BackgroundShellSummary:
    """Aggregated result the probe writes to ``summary.json``."""

    schema: str = SUMMARY_SCHEMA
    mode: str = ""
    launches: list[_LaunchRecord] = field(default_factory=list)
    foreground_mount_s: list[float] = field(default_factory=list)
    total_duration_s: float = 0.0
    foreground_p95_mount_s: float = 0.0

    def to_payload(self) -> dict[str, object]:
        return {
            "schema": self.schema,
            "mode": self.mode,
            "launches": [
                {
                    "index": record.index,
                    "started_at": record.started_at,
                    "completed_at": record.completed_at,
                    "status": record.status,
                    "exit_code": record.exit_code,
                    "changed_paths_count": record.changed_paths_count,
                    "cancelled": record.cancelled,
                    "error": record.error,
                }
                for record in self.launches
            ],
            "foreground_mount_s": list(self.foreground_mount_s),
            "total_duration_s": self.total_duration_s,
            "foreground_p95_mount_s": self.foreground_p95_mount_s,
        }


def _percentile(values: Iterable[float], pct: float) -> float:
    """Simple ``pct`` percentile (linear interpolation). Empty list → 0.0."""
    samples = sorted(values)
    if not samples:
        return 0.0
    if len(samples) == 1:
        return samples[0]
    rank = (pct / 100.0) * (len(samples) - 1)
    lo = int(rank)
    hi = min(lo + 1, len(samples) - 1)
    frac = rank - lo
    return samples[lo] * (1 - frac) + samples[hi] * frac


def _caller(agent_id: str = "background-shell-probe") -> SandboxCaller:
    return SandboxCaller(agent_id=agent_id)


async def seed_workspace(sandbox_id: str) -> None:
    """Pre-create ``ROOT`` so subsequent writes don't race on ``mkdir``."""
    request = ShellRequest(
        command=f"mkdir -p {ROOT}",
        cwd=".",
        timeout=DEFAULT_TIMEOUT_S,
        background=False,
        caller=_caller(),
        description="background_shell.seed",
    )
    await sandbox_api.shell(sandbox_id, request)


async def run_background_shell_golden_probe(
    *,
    sandbox_id: str,
    launch_count: int = 3,
    sleep_s: int = 5,
) -> BackgroundShellSummary:
    """T1 surface: N concurrent background launches; wait for natural exit."""
    summary = BackgroundShellSummary(mode="golden")
    started = time.monotonic()

    async def _one(index: int) -> _LaunchRecord:
        record = _LaunchRecord(index=index, started_at=time.monotonic())
        request = ShellRequest(
            command=f"sleep {sleep_s}; echo done-{index}",
            cwd=".",
            timeout=DEFAULT_TIMEOUT_S,
            background=True,
            caller=_caller(f"background-shell-probe.golden.{index}"),
            description=f"background_shell.golden.{index}",
        )
        result = await sandbox_api.shell(sandbox_id, request)
        _record_from_result(record, result)
        return record

    results = await asyncio.gather(
        *(_one(i) for i in range(launch_count)),
        return_exceptions=False,
    )
    summary.launches.extend(results)
    summary.total_duration_s = time.monotonic() - started
    return summary


async def run_background_shell_cancel_probe(
    *,
    sandbox_id: str,
    launch_count: int = 3,
    cancel_after_s: float = DEFAULT_CANCEL_AFTER_S,
    sleep_s: int = DEFAULT_BACKGROUND_SLEEP_S,
) -> BackgroundShellSummary:
    """T2 surface: launch + cancel mid-flight; assert no leftover state.

    Drives the asyncio CancelledError path of ``_shell_background_dispatch``:
    each launch is wrapped in ``asyncio.wait_for`` with a deadline shorter
    than the shell's own runtime, so the host-side cancel + reap chain
    fires exactly once per launch.
    """
    summary = BackgroundShellSummary(mode="cancel")
    started = time.monotonic()

    async def _one(index: int) -> _LaunchRecord:
        record = _LaunchRecord(index=index, started_at=time.monotonic())
        request = ShellRequest(
            command=f"sleep {sleep_s}; echo done-{index}",
            cwd=".",
            timeout=DEFAULT_TIMEOUT_S,
            background=True,
            caller=_caller(f"background-shell-probe.cancel.{index}"),
            description=f"background_shell.cancel.{index}",
        )
        try:
            result = await asyncio.wait_for(
                sandbox_api.shell(sandbox_id, request),
                timeout=cancel_after_s,
            )
            _record_from_result(record, result)
        except asyncio.TimeoutError:
            # asyncio.wait_for cancels the underlying task → host
            # _shell_background_dispatch publishes SHELL_CANCELLED + reap.
            record.cancelled = True
            record.status = "cancelled"
            record.completed_at = time.monotonic()
        return record

    results = await asyncio.gather(
        *(_one(i) for i in range(launch_count)),
        return_exceptions=False,
    )
    summary.launches.extend(results)
    summary.total_duration_s = time.monotonic() - started
    return summary


async def run_background_shell_interleave_probe(
    *,
    sandbox_id: str,
    foreground_count: int = DEFAULT_INTERLEAVE_COUNT,
    background_sleep_s: int = DEFAULT_BACKGROUND_SLEEP_S,
) -> BackgroundShellSummary:
    """T3 surface: 1 background + M foreground; record foreground mount times.

    The background shell is launched without awaiting completion; the
    foreground reads run sequentially against fresh leases. AC-3 asserts
    that foreground p95 ``command_exec.mount_workspace_s`` stays low even
    while a long-running background lease is held.
    """
    summary = BackgroundShellSummary(mode="interleave")
    started = time.monotonic()

    bg_record = _LaunchRecord(index=0, started_at=time.monotonic())
    bg_request = ShellRequest(
        command=f"sleep {background_sleep_s}; echo bg-done",
        cwd=".",
        timeout=DEFAULT_TIMEOUT_S,
        background=True,
        caller=_caller("background-shell-probe.interleave.bg"),
        description="background_shell.interleave.bg",
    )
    bg_task = asyncio.create_task(sandbox_api.shell(sandbox_id, bg_request))

    try:
        for index in range(foreground_count):
            fg_request = ShellRequest(
                command=f"echo fg-{index}",
                cwd=".",
                timeout=DEFAULT_TIMEOUT_S,
                background=False,
                caller=_caller(f"background-shell-probe.interleave.fg.{index}"),
                description=f"background_shell.interleave.fg.{index}",
            )
            t0 = time.monotonic()
            fg_result = await sandbox_api.shell(sandbox_id, fg_request)
            mount_s = _mount_s_from_result(fg_result) or (time.monotonic() - t0)
            summary.foreground_mount_s.append(mount_s)
    finally:
        # Let the background shell finish (or cancel if the test is tearing
        # down quickly). The host-side _shell_background_dispatch handles
        # its own cleanup on CancelledError.
        try:
            bg_result = await asyncio.wait_for(bg_task, timeout=background_sleep_s + 30)
            _record_from_result(bg_record, bg_result)
        except asyncio.TimeoutError:
            bg_task.cancel()
            bg_record.cancelled = True
            bg_record.status = "cancelled"
            bg_record.completed_at = time.monotonic()

    summary.launches.append(bg_record)
    summary.foreground_p95_mount_s = _percentile(summary.foreground_mount_s, 95.0)
    summary.total_duration_s = time.monotonic() - started
    return summary


async def write_summary(sandbox_id: str, summary: BackgroundShellSummary) -> str:
    """Persist the summary to ``SUMMARY_PATH`` via the sandbox write API.

    Using shell + heredoc keeps the contract identical to the heavy_io
    probe (no host-side filesystem assumptions). The summary path is what
    the live tests read back via ``sandbox_api.read_file``.
    """
    payload = json.dumps(summary.to_payload(), indent=2, sort_keys=True) + "\n"
    encoded = payload.replace("'", "'\\''")
    command = (
        f"mkdir -p {ROOT} && "
        f"printf '%s' '{encoded}' > {SUMMARY_PATH}"
    )
    request = ShellRequest(
        command=command,
        cwd=".",
        timeout=DEFAULT_TIMEOUT_S,
        background=False,
        caller=_caller("background-shell-probe.write_summary"),
        description="background_shell.write_summary",
    )
    await sandbox_api.shell(sandbox_id, request)
    return SUMMARY_PATH


def _record_from_result(record: _LaunchRecord, result: ShellResult) -> None:
    record.completed_at = time.monotonic()
    record.status = str(getattr(result, "status", "") or "")
    record.exit_code = int(getattr(result, "exit_code", -1) or -1)
    record.changed_paths_count = len(getattr(result, "changed_paths", ()) or ())
    if not result.success and record.status != "cancelled":
        record.error = str(getattr(result, "stderr", "") or "")[:200]


def _mount_s_from_result(result: ShellResult) -> float | None:
    timings = getattr(result, "timings", None)
    if not isinstance(timings, dict):
        return None
    for key in (
        "command_exec.mount_workspace_s",
        "api.shell.dispatch_total_s",
    ):
        value = timings.get(key)
        if value is None:
            continue
        try:
            return float(value)
        except (TypeError, ValueError):
            continue
    return None


__all__ = [
    "BackgroundShellSummary",
    "DEFAULT_BACKGROUND_SLEEP_S",
    "DEFAULT_CANCEL_AFTER_S",
    "DEFAULT_INTERLEAVE_COUNT",
    "DEFAULT_TIMEOUT_S",
    "ROOT",
    "SUMMARY_PATH",
    "SUMMARY_SCHEMA",
    "run_background_shell_cancel_probe",
    "run_background_shell_golden_probe",
    "run_background_shell_interleave_probe",
    "seed_workspace",
    "write_summary",
]
