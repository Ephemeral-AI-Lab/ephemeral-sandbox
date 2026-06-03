"""Helpers for integrated public-tool live tests."""

from __future__ import annotations

import asyncio
import json
import os
import shlex
import time
from collections.abc import Awaitable, Iterable, Sequence
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import TypeVar
from uuid import uuid4

import pytest

from sandbox.api import (
    EditFileResult,
    ExecCommandResult,
    GuardedResultBase,
    RawExecResult,
    ReadFileResult,
    WriteFileResult,
)

from .sandbox_fixture import SandboxHandle


ApiResult = ReadFileResult | WriteFileResult | EditFileResult | ExecCommandResult
TApiResult = TypeVar("TApiResult", bound=ApiResult)


@dataclass(frozen=True)
class RuntimeCallMetric:
    label: str
    op: str
    success: bool
    status: str
    elapsed_ms: float
    changed_paths: tuple[str, ...]
    conflict_reason: str | None
    timings: dict[str, float]


_TIMING_JSONL_ENV = "EPHEMERALOS_LIVE_E2E_TIMING_JSONL"
_TIMING_SCHEMA = "sandbox.live_e2e.per_call_timings.v1"
_FIXED_TIMING_KEYS = (
    "api.read.layer_stack_read_s",
    "api.read.total_s",
    "api.write.prepare_s",
    "api.write.commit_s",
    "api.write.flock_wait_s",
    "api.write.process_gate_wait_s",
    "api.write.total_s",
    "api.edit.prepare_s",
    "api.edit.commit_s",
    "api.edit.flock_wait_s",
    "api.edit.process_gate_wait_s",
    "api.edit.total_s",
    "api.shell.dispatch_total_s",
    "api.shell.total_s",
    "api.shell.overlay_s",
    "api.shell.occ_apply_s",
    "api.shell.prepare_s",
    "api.shell.commit_s",
    "api.shell.process_gate_wait_s",
    "api.shell.flock_wait_s",
    "api.shell.overlay_capture_to_changes_s",
    "overlay.mount.materialize_lower_s",
    "overlay.mount.copy_lower_to_merged_s",
    "overlay.run_command_s",
    "overlay.capture_changes_s",
    "layer_stack.transaction.lock_wait_s",
    "layer_stack.transaction.lock_held_s",
    "runtime.boot_to_dispatch_s",
    "runtime.dispatch_s",
    "gitignore.cache_hits_total",
    "gitignore.cache_misses_total",
)
_TIMING_PREFIXES = ("occ.prepare.", "occ.commit.")
_RUN_ID = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ") + f"-{os.getpid()}"
_TIMING_JSONL_PATH: Path | None = None


def token(label: str) -> str:
    safe = "".join(ch if ch.isalnum() or ch in "-_" else "-" for ch in label)
    return f"{safe}-{uuid4().hex[:10]}"


def tmp_path(name: str) -> str:
    return f"/tmp/eos-phase3-{name}"


def q(value: str) -> str:
    return shlex.quote(value)


async def remove_tmp(handle: SandboxHandle, *paths: str) -> None:
    if not paths:
        return
    command = "rm -f -- " + " ".join(q(path) for path in paths)
    result = await handle.raw_exec(handle.sandbox_id, command, timeout=15)
    if result.exit_code != 0:
        pytest.fail(f"side-channel cleanup failed: {result.stderr or result.stdout}")


async def touch_tmp(handle: SandboxHandle, path: str) -> None:
    result = await handle.raw_exec(handle.sandbox_id, f"touch -- {q(path)}", timeout=15)
    if result.exit_code != 0:
        pytest.fail(f"side-channel touch failed: {path}: {result.stderr or result.stdout}")


async def wait_for_tmp(
    handle: SandboxHandle,
    path: str,
    *,
    timeout_s: float = 15.0,
) -> None:
    deadline = time.monotonic() + timeout_s
    while time.monotonic() < deadline:
        result = await handle.raw_exec(handle.sandbox_id, f"test -f {q(path)}", timeout=5)
        if result.exit_code == 0:
            return
        await asyncio.sleep(0.05)
    pytest.fail(f"timed out waiting for side-channel file: {path}")


async def wait_for_tmps(
    handle: SandboxHandle,
    paths: Sequence[str],
    *,
    timeout_s: float = 15.0,
) -> None:
    await asyncio.gather(
        *(wait_for_tmp(handle, path, timeout_s=timeout_s) for path in paths)
    )


async def assert_read(
    handle: SandboxHandle,
    path: str,
    expected: str,
) -> ReadFileResult:
    result = await handle.tool.read_file(path)
    assert result.success
    assert result.exists
    assert result.content == expected
    return result


def assert_committed(result: GuardedResultBase, *, path: str | None = None) -> None:
    assert result.success, result.conflict_reason
    assert result.status in {"committed", "accepted", "ok"}, result
    if path is not None:
        assert path in result.changed_paths


def assert_rejected(result: GuardedResultBase, *, path: str | None = None) -> None:
    assert not result.success, result
    assert result.changed_paths == ()
    assert result.conflict_reason
    if path is not None and result.conflict is not None:
        assert result.conflict.conflict_file in {path, None}


def metric_for(label: str, result: ApiResult, elapsed_ms: float) -> RuntimeCallMetric:
    return RuntimeCallMetric(
        label=label,
        op=_op_name(result),
        success=result.success,
        status=_status(result),
        elapsed_ms=elapsed_ms,
        changed_paths=_changed_paths(result),
        conflict_reason=_conflict_reason(result),
        timings=dict(result.timings),
    )


def percentile(values: Sequence[float], pct: float) -> float:
    if not values:
        return 0.0
    ordered = sorted(values)
    if len(ordered) == 1:
        return ordered[0]
    rank = (len(ordered) - 1) * (pct / 100.0)
    lower = int(rank)
    upper = min(lower + 1, len(ordered) - 1)
    weight = rank - lower
    return ordered[lower] * (1.0 - weight) + ordered[upper] * weight


def summarize_calls(metrics: Sequence[RuntimeCallMetric]) -> dict[str, object]:
    elapsed = [metric.elapsed_ms for metric in metrics]
    return {
        "calls": len(metrics),
        "successes": sum(1 for metric in metrics if metric.success),
        "rejects": sum(1 for metric in metrics if not metric.success),
        "p50_ms": round(percentile(elapsed, 50), 3),
        "p99_ms": round(percentile(elapsed, 99), 3),
        "max_ms": round(max(elapsed, default=0.0), 3),
        "changed_paths": sorted(
            {path for metric in metrics for path in metric.changed_paths}
        ),
        "conflicts": sorted(
            {
                str(metric.conflict_reason)
                for metric in metrics
                if metric.conflict_reason
            }
        ),
    }


def emit_metric(label: str, payload: dict[str, object]) -> None:
    print(
        "PHASE3_METRIC "
        + json.dumps({"label": label, **payload}, sort_keys=True),
        flush=True,
    )


def timing_jsonl_path() -> Path:
    global _TIMING_JSONL_PATH
    if _TIMING_JSONL_PATH is not None:
        return _TIMING_JSONL_PATH
    configured = os.environ.get(_TIMING_JSONL_ENV)
    if configured:
        path = Path(configured)
    else:
        path = (
            Path.cwd()
            / ".omc"
            / "results"
            / f"live-e2e-phase3-per-call-timings-{_RUN_ID}.jsonl"
        )
    path.parent.mkdir(parents=True, exist_ok=True)
    _TIMING_JSONL_PATH = path
    return path


def write_timing_record(metric: RuntimeCallMetric) -> None:
    record: dict[str, object] = {
        "schema": _TIMING_SCHEMA,
        "run_id": _RUN_ID,
        "label": metric.label,
        "op": metric.op,
        "success": metric.success,
        "status": metric.status,
        "wall_ms": round(metric.elapsed_ms, 3),
        "changed_paths": list(metric.changed_paths),
        "conflict_reason": metric.conflict_reason,
    }
    for key in _FIXED_TIMING_KEYS:
        record[key] = _seconds(metric.timings.get(key))
    for key in sorted(metric.timings):
        if key.startswith(_TIMING_PREFIXES):
            record[key] = _seconds(metric.timings[key])
    record["timings"] = {
        key: _seconds(value)
        for key, value in sorted(metric.timings.items())
    }

    with timing_jsonl_path().open("a", encoding="utf-8") as file:
        file.write(json.dumps(record, sort_keys=True, separators=(",", ":")))
        file.write("\n")


async def timed_call(
    label: str,
    awaitable: Awaitable[TApiResult],
) -> tuple[TApiResult, RuntimeCallMetric]:
    start = time.perf_counter()
    result = await awaitable
    elapsed_ms = (time.perf_counter() - start) * 1000.0
    metric = metric_for(label, result, elapsed_ms)
    write_timing_record(metric)
    return result, metric


async def timed_raw_exec(
    label: str,
    handle: SandboxHandle,
    command: str,
    *,
    cwd: str | None = None,
    timeout: int | None = None,
    changed_paths: Sequence[str] = (),
) -> tuple[RawExecResult, RuntimeCallMetric]:
    start = time.perf_counter()
    result = await handle.raw_exec(
        handle.sandbox_id,
        command,
        cwd=cwd,
        timeout=timeout,
    )
    elapsed_ms = (time.perf_counter() - start) * 1000.0
    metric = RuntimeCallMetric(
        label=label,
        op="process_exec",
        success=result.success,
        status="ok" if result.success else "error",
        elapsed_ms=elapsed_ms,
        changed_paths=tuple(changed_paths if result.success else ()),
        conflict_reason=None if result.success else result.stderr or result.stdout,
        timings=dict(result.timings),
    )
    write_timing_record(metric)
    return result, metric


def paths_visible_summary(reads: Iterable[ReadFileResult]) -> dict[str, int]:
    rows = tuple(reads)
    return {
        "reads": len(rows),
        "visible": sum(1 for result in rows if result.success and result.exists),
        "missing": sum(1 for result in rows if result.success and not result.exists),
    }


def _op_name(result: ApiResult) -> str:
    if isinstance(result, ReadFileResult):
        return "read_file"
    if isinstance(result, ExecCommandResult):
        return "shell"
    if isinstance(result, EditFileResult):
        return "edit_file"
    if isinstance(result, WriteFileResult):
        return "write_file"
    return type(result).__name__


def _status(result: ApiResult) -> str:
    if isinstance(result, ReadFileResult):
        if not result.success:
            return "error"
        return "ok" if result.exists else "missing"
    return result.status


def _changed_paths(result: ApiResult) -> tuple[str, ...]:
    if isinstance(result, ReadFileResult):
        return ()
    return result.changed_paths


def _conflict_reason(result: ApiResult) -> str | None:
    if isinstance(result, ReadFileResult):
        return None
    return result.conflict_reason


def _seconds(value: float | None) -> float | None:
    if value is None:
        return None
    return round(float(value), 6)


__all__ = [
    "RuntimeCallMetric",
    "assert_committed",
    "assert_read",
    "assert_rejected",
    "emit_metric",
    "metric_for",
    "paths_visible_summary",
    "percentile",
    "q",
    "remove_tmp",
    "summarize_calls",
    "timed_call",
    "timed_raw_exec",
    "timing_jsonl_path",
    "tmp_path",
    "token",
    "touch_tmp",
    "wait_for_tmp",
    "wait_for_tmps",
]
