"""K=1000 × concurrency {1, 5, 10, 20} live benchmark.

Phase 3.5 spike: how does a 1000-file capture scale when N shell calls
hit the daemon at the same time? Each call writes K=1000 × 64-byte
files into its own subdirectory under ``tracked/load/k1000_c{N}/`` so
calls don't collide on path.

For each c ∈ {1, 5, 10, 20}, runs c concurrent shell calls (each one
creating 1000 files), measures wall_ms and per-call commit_s, and
emits ``phase09.k1000_concurrency.v1`` JSONL rows.

Pass bars (informational only — this is a measurement, not a falsifier):
- every call must succeed
- parallel_efficiency = (best_serial_wall_ms / batch_wall_ms × c) recorded
"""

from __future__ import annotations

import statistics
import time
from pathlib import Path

import pytest

from .._harness.concurrency import gather_with_barrier
from .._harness.integrated_cases import emit_metric, timed_call
from .._harness.large_capture_workload import build_sized_capture
from .._harness.phase05_public_file_ops import seed_phase05_imported_base
from .._harness.sandbox_fixture import SandboxHandle
from .._harness.streaming_artifact import (
    resolve_run_id as _resolve_run_id,
    stream_row as _stream_row,
)


pytestmark = pytest.mark.asyncio

_BASE = "tracked/load/k1000_concurrency"
_K = 1000
_FILE_SIZE = 64
_CONCURRENCY = (1, 5, 10, 20)


def _artifact_path() -> Path:
    target = (
        Path.cwd()
        / ".omc"
        / "results"
        / f"phase09-k1000-concurrency-{_resolve_run_id()}.jsonl"
    )
    target.parent.mkdir(parents=True, exist_ok=True)
    return target


async def test_phase09_k1000_concurrency(
    workspace_base_sandbox: SandboxHandle,
) -> None:
    handle = workspace_base_sandbox
    await seed_phase05_imported_base(handle)
    await handle.tool.shell(
        f"rm -rf {_BASE}; mkdir -p {_BASE}",
        timeout=30,
        description="phase09 k1000_concurrency reset",
    )

    artifact = _artifact_path()
    # Each invocation collects fresh data; batch-level concurrency
    # measurements are not meaningfully resumable mid-batch.
    if artifact.exists():
        artifact.unlink()
    rows: list[dict[str, object]] = []
    summaries: list[dict[str, object]] = []

    serial_wall_ms_for_efficiency: float | None = None

    for c in _CONCURRENCY:
        cell_dir_template = f"{_BASE}/c{c}"
        await handle.tool.shell(
            f"rm -rf {cell_dir_template}; mkdir -p {cell_dir_template}",
            timeout=30,
            description=f"phase09 k1000 c{c} reset",
        )

        async def _make_call(call_index: int, _c: int = c) -> dict[str, object]:
            cell_dir = f"{cell_dir_template}/call_{call_index:04d}"
            label = f"phase09.k1000.c{_c}.call{call_index}"
            command = build_sized_capture(cell_dir, _K, _FILE_SIZE)
            result, metric = await timed_call(
                label,
                handle.tool.shell(command, timeout=600, description=label),
            )
            return {
                "call_index": call_index,
                "success": bool(result.success),
                "wall_ms": metric.elapsed_ms,
                "commit_s": float(metric.timings.get("occ.commit.total_s", 0.0)),
                "stager_s": float(
                    metric.timings.get("occ.commit.stager_write_total_s", 0.0)
                ),
                "capture_s": float(
                    metric.timings.get("command_exec.capture_upperdir_s", 0.0)
                ),
                "occ_apply_s": float(
                    metric.timings.get("command_exec.occ_apply_s", 0.0)
                ),
            }

        batch_start = time.perf_counter()
        per_call = await gather_with_barrier(
            [(lambda idx=i: _make_call(idx)) for i in range(c)]
        )
        batch_wall_ms = (time.perf_counter() - batch_start) * 1000.0

        all_success = all(r["success"] for r in per_call)
        if not all_success:
            failures = [r for r in per_call if not r["success"]]
            assert False, f"phase09 k1000 c{c}: {len(failures)} call(s) failed"

        walls = sorted(r["wall_ms"] for r in per_call)
        commits = sorted(r["commit_s"] for r in per_call)
        median_wall = statistics.median(walls)
        median_commit = statistics.median(commits)
        ops_per_s = c / (batch_wall_ms / 1000.0) if batch_wall_ms > 0 else 0.0

        # Use c=1's wall_ms as the serial baseline for parallel_efficiency.
        if c == 1:
            serial_wall_ms_for_efficiency = batch_wall_ms
        if serial_wall_ms_for_efficiency and batch_wall_ms > 0:
            parallel_efficiency = (
                serial_wall_ms_for_efficiency / batch_wall_ms
                if c == 1
                else (serial_wall_ms_for_efficiency / batch_wall_ms)
            )
        else:
            parallel_efficiency = 0.0

        summary = {
            "schema": "phase09.k1000_concurrency.v1",
            "concurrency": c,
            "k": _K,
            "file_size_bytes": _FILE_SIZE,
            "batch_wall_ms": round(batch_wall_ms, 3),
            "ops_per_s": round(ops_per_s, 3),
            "median_call_wall_ms": round(median_wall, 3),
            "median_call_commit_s": round(median_commit, 6),
            "p99_call_wall_ms": round(walls[-1], 3),
            "parallel_efficiency": round(parallel_efficiency, 3),
            "calls": c,
            "calls_succeeded": sum(1 for r in per_call if r["success"]),
        }
        for r in per_call:
            row = dict(r)
            row["concurrency"] = c
            row["schema"] = "phase09.k1000_concurrency.call.v1"
            _stream_row(artifact, row)
            rows.append(row)
        summaries.append(summary)
        _stream_row(artifact, summary)
        emit_metric(f"phase09.k1000.c{c}.summary", summary)

    print(f"\n[phase09:k1000_concurrency] artifact={artifact}")
    print(
        "\n  c | batch_ms | ops/s | median_wall | p99_wall | median_commit | parallel_eff"
    )
    for s in summaries:
        print(
            f"  {s['concurrency']:>1} | "
            f"{s['batch_wall_ms']:>8.0f} | "
            f"{s['ops_per_s']:>5.2f} | "
            f"{s['median_call_wall_ms']:>11.0f} | "
            f"{s['p99_call_wall_ms']:>8.0f} | "
            f"{s['median_call_commit_s']:>13.4f} | "
            f"{s['parallel_efficiency']:>11.3f}"
        )
