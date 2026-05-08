"""Phase 09 — size × concurrency matrix (progressive-tiers Phase D, T-D1).

12 cells: ``file_size_bytes ∈ {64, 4096, 65536} × c ∈ {1, 5, 10, 20}``.
Each cell launches ``c`` concurrent shell calls (gather_with_barrier).
Each call writes ``K=64`` files of the chosen size into its own
subdirectory under ``tracked/load/phase09_szc/`` so calls don't
collide on path.

The test follows the per-cell streaming + resume contract from
``progressive-live-test-tiers-design-20260508.md`` §§4-5: each cell's
JSONL row is appended + flushed + fsynced to
``.omc/results/phase09-size-x-concurrency-<run_id>.jsonl`` BEFORE the
next cell starts. A kill-9 mid-matrix preserves prior cells.

Strict pass bars per cell:

* every concurrent call must succeed
* per-cell median commit_s ≤ 3 × the c=1 baseline at the same file size

End-of-matrix summary row asserts ``failed_cells == 0``.
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
    load_prior_data_rows as _load_prior_data_rows,
    resolve_run_id as _resolve_run_id,
    rewrite_artifact as _rewrite_artifact,
    stream_row as _stream_row,
)


pytestmark = pytest.mark.asyncio


_BASE = "tracked/load/phase09_szc"
_K = 64
_SIZES = (64, 4_096, 65_536)
_CONCURRENCY = (1, 5, 10, 20)


def _artifact_path() -> Path:
    target = (
        Path.cwd()
        / ".omc"
        / "results"
        / f"phase09-size-x-concurrency-{_resolve_run_id()}.jsonl"
    )
    target.parent.mkdir(parents=True, exist_ok=True)
    return target


async def _run_one_call(
    handle: SandboxHandle,
    *,
    cell_dir_template: str,
    call_index: int,
    file_size: int,
    label: str,
) -> dict[str, object]:
    cell_dir = f"{cell_dir_template}/call_{call_index:04d}"
    command = build_sized_capture(cell_dir, _K, file_size)
    result, metric = await timed_call(
        label,
        handle.tool.shell(command, timeout=600, description=label),
    )
    return {
        "call_index": call_index,
        "success": bool(result.success),
        "wall_ms": metric.elapsed_ms,
        "commit_s": float(metric.timings.get("occ.commit.total_s", 0.0)),
        "stager_s": float(metric.timings.get("occ.commit.stager_write_total_s", 0.0)),
        "capture_s": float(metric.timings.get("command_exec.capture_upperdir_s", 0.0)),
    }


async def test_phase09_size_x_concurrency(
    workspace_base_sandbox: SandboxHandle,
) -> None:
    handle = workspace_base_sandbox
    await seed_phase05_imported_base(handle)
    await handle.tool.shell(
        f"rm -rf {_BASE}; mkdir -p {_BASE}",
        timeout=30,
        description="phase09 size_x_concurrency reset",
    )

    artifact = _artifact_path()
    prior_rows = _load_prior_data_rows(artifact)
    completed: set[str] = {
        str(row["cell_id"])
        for row in prior_rows
        if row.get("cell_id") and row.get("passed") is True
    }
    rows: list[dict[str, object]] = list(prior_rows)
    run_id = _resolve_run_id()

    # Track c=1 medians per file size so per-size pass bars (median ≤
    # 3× c=1 baseline) can be evaluated, including across resumes.
    c1_baseline_per_size: dict[int, float] = {}
    for row in prior_rows:
        axes = row.get("axis_values", {}) if isinstance(row.get("axis_values"), dict) else {}
        if axes.get("c") == 1 and row.get("passed") is True:
            commit_median = float(
                row.get("occ_timings", {}).get("median_commit_s", 0.0)
            )
            c1_baseline_per_size[int(axes["file_size_bytes"])] = commit_median

    matrix_start = time.perf_counter()

    for size in _SIZES:
        for c in _CONCURRENCY:
            cell_id = f"size{size}_c{c}"
            if cell_id in completed:
                continue
            cell_dir_template = f"{_BASE}/size_{size}/c{c}"
            await handle.tool.shell(
                f"rm -rf {cell_dir_template}; mkdir -p {cell_dir_template}",
                timeout=30,
                description=f"phase09 size_x_concurrency reset {cell_id}",
            )

            label = f"phase09.size_x_c.{cell_id}"
            batch_start = time.perf_counter()
            per_call = await gather_with_barrier(
                [
                    (
                        lambda idx=i: _run_one_call(
                            handle,
                            cell_dir_template=cell_dir_template,
                            call_index=idx,
                            file_size=size,
                            label=f"{label}.call{idx}",
                        )
                    )
                    for i in range(c)
                ]
            )
            batch_wall_ms = (time.perf_counter() - batch_start) * 1000.0

            all_succeeded = all(r["success"] for r in per_call)
            commits = sorted(r["commit_s"] for r in per_call)
            walls = sorted(r["wall_ms"] for r in per_call)
            median_commit = statistics.median(commits) if commits else 0.0
            median_wall = statistics.median(walls) if walls else 0.0
            p99_wall = walls[-1] if walls else 0.0

            if c == 1 and all_succeeded:
                c1_baseline_per_size[size] = median_commit

            baseline = c1_baseline_per_size.get(size)
            passed = all_succeeded
            failure_reason: object | None = None
            if not all_succeeded:
                failed = [r for r in per_call if not r["success"]]
                failure_reason = {
                    "category": "call_failed",
                    "failed_call_count": len(failed),
                }
            elif baseline is not None and baseline > 0 and median_commit > 3 * baseline:
                passed = False
                failure_reason = {
                    "category": "median_commit_regression",
                    "baseline_s": baseline,
                    "observed_s": median_commit,
                    "threshold_s": 3 * baseline,
                }

            row: dict[str, object] = {
                "schema": "phase09.size_x_concurrency.v1",
                "matrix": "size_x_concurrency",
                "cell_id": cell_id,
                "axis_values": {
                    "file_size_bytes": size,
                    "c": c,
                    "k": _K,
                },
                "passed": passed,
                "failure_reason": failure_reason,
                "wall_ms": round(batch_wall_ms, 3),
                "occ_timings": {
                    "median_commit_s": round(median_commit, 6),
                    "p99_wall_ms": round(p99_wall, 3),
                    "median_wall_ms": round(median_wall, 3),
                },
                "correctness": {
                    "all_succeeded": all_succeeded,
                    "calls": c,
                    "calls_succeeded": sum(1 for r in per_call if r["success"]),
                },
                "run_id": run_id,
            }
            _stream_row(artifact, row)
            rows.append(row)
            emit_metric(label, row)

    elapsed = time.perf_counter() - matrix_start
    failed = [r for r in rows if not r.get("passed", False)]
    summary: dict[str, object] = {
        "schema": "phase09.size_x_concurrency.summary.v1",
        "matrix": "size_x_concurrency",
        "run_id": run_id,
        "total_cells": len(rows),
        "passed_cells": len(rows) - len(failed),
        "failed_cells": len(failed),
        "failed_cell_ids": [str(r["cell_id"]) for r in failed],
        "elapsed_total_s": round(elapsed, 3),
        "artifact": str(artifact),
    }
    _rewrite_artifact(artifact, rows, summary)
    print(f"\n[phase09:size_x_concurrency] artifact={artifact}")
    emit_metric("phase09.size_x_concurrency.summary", summary)
    assert summary["failed_cells"] == 0, (
        f"phase09 size×concurrency failed_cells={summary['failed_cells']} "
        f"failed_ids={summary['failed_cell_ids']} artifact={artifact}"
    )
