"""Scenario 1 perf: mixed-op concurrent workload scaling (live-e2e).

The mock half (``background_tool/test_background_mixed_op_concurrent``) proves
correctness AND, via ``assert_background_performance_artifacts``, the O(1)
per-call workspace-resource bound. This live-e2e cell adds the missing
*scaling curve*: the same heterogeneous workload (a python edit-loop + a file
write per worker, exercising the overlay→OCC publish path) fanned out at
concurrency {1, 5, 10} through the real sandbox, sampling daemon resources and
``/dev/shm`` run-dir bounds before/after each level and recording a JSONL row
per level (``EOS_TIER_RUN_ID``-pinned for the tiered runner).

Gates (D9 — recorded metrics with thresholds, generous to stay portable across
CI hosts; tight absolute budgets are Tier-9's job):
  * per-call p99 at each level ≤ ``max(3 × concurrency-1 baseline, floor)``.
  * ``/dev/shm/eos-command-exec`` stays bounded (phase08 invariant) — only
    in-flight run-dirs, never an O(calls) leak.
  * daemon RSS / open-fd deltas across the whole run stay within a no-leak
    ceiling.
"""

from __future__ import annotations

import time
from pathlib import Path

import pytest

from sandbox.api import ExecCommandResult

from .._harness.concurrency import gather_with_barrier
from .._harness.integrated_cases import (
    RuntimeCallMetric,
    assert_committed,
    emit_metric,
    percentile,
    q,
    timed_call,
)
from .._harness.sandbox_fixture import SandboxHandle
from .._harness.streaming_artifact import resolve_run_id, stream_row


pytestmark = pytest.mark.asyncio

_CONCURRENCY_LEVELS = (1, 5, 10)
_EDIT_LINES = 40
_DEV_SHM_DIR = "/dev/shm/eos-command-exec"
_DEV_SHM_RUN_DIR_CEILING = 16  # in-flight run-dirs only; never O(calls)
_DEV_SHM_BYTES_CEILING = 64 * 1024 * 1024
_LATENCY_FLOOR_MS = 750.0
_LATENCY_RATIO = 3.0
_RSS_DELTA_CEILING_KB = 512 * 1024  # 512 MiB daemon growth ⇒ leak
_FD_DELTA_CEILING = 256


def _artifact_path() -> Path:
    target = (
        Path.cwd()
        / ".omc"
        / "results"
        / f"scenario1-mixed-op-scaling-{resolve_run_id()}.jsonl"
    )
    target.parent.mkdir(parents=True, exist_ok=True)
    return target


async def _probe_resources(handle: SandboxHandle) -> dict[str, int]:
    """Sample daemon RSS/threads/fds/mounts + /dev/shm bounds via raw_exec.

    Mirrors ``resource_metrics``' §3.5 fields but reads the *daemon* process
    (the thing under load) rather than an in-sandbox probe process, and travels
    the same Daytona path the daemon uses (phase08 pattern).
    """
    cmd = (
        "set +e; "
        "pid=$(pgrep -f '^.*python.*-m sandbox\\.daemon' | head -1); "
        'rss=0; hwm=0; threads=0; fds=0; mounts=0; '
        'if [ -n "$pid" ]; then '
        '  rss=$(awk \'/^VmRSS:/{print $2}\' /proc/$pid/status 2>/dev/null); '
        '  hwm=$(awk \'/^VmHWM:/{print $2}\' /proc/$pid/status 2>/dev/null); '
        '  threads=$(awk \'/^Threads:/{print $2}\' /proc/$pid/status 2>/dev/null); '
        '  fds=$(ls /proc/$pid/fd 2>/dev/null | wc -l); '
        '  mounts=$(wc -l < /proc/$pid/mounts 2>/dev/null); '
        "fi; "
        f'shm_count=0; shm_bytes=0; '
        f'if [ -d {_DEV_SHM_DIR} ]; then '
        f'  shm_count=$(find {_DEV_SHM_DIR} -mindepth 2 -maxdepth 2 -type d 2>/dev/null | wc -l); '
        f"  shm_bytes=$(du -sb {_DEV_SHM_DIR} 2>/dev/null | awk '{{print $1}}'); "
        "fi; "
        'printf "rss=%s\\nhwm=%s\\nthreads=%s\\nfds=%s\\nmounts=%s\\nshm_count=%s\\nshm_bytes=%s\\n" '
        '"${rss:-0}" "${hwm:-0}" "${threads:-0}" "${fds:-0}" "${mounts:-0}" '
        '"${shm_count:-0}" "${shm_bytes:-0}"'
    )
    result = await handle.raw_exec(handle.sandbox_id, cmd, timeout=30)
    out: dict[str, int] = {}
    for line in (result.stdout or "").splitlines():
        if "=" in line:
            key, _, value = line.partition("=")
            out[key.strip()] = int(value.strip() or "0") if value.strip().isdigit() else 0
    return out


async def _run_mixed_level(
    handle: SandboxHandle,
    concurrency: int,
) -> tuple[list[RuntimeCallMetric], float]:
    factories = []
    for index in range(concurrency):
        path = f"dist/scenario1/c{concurrency:02d}-{index:02d}.txt"
        command = (
            "set -e; "
            "mkdir -p dist/scenario1; "
            f"python3 -c \"open('{path}','w').write("
            f"''.join('line-%d\\n' % i for i in range({_EDIT_LINES})))\"; "
            f"printf 'tail-{index}\\n' >> {q(path)}"
        )

        async def run_worker(
            index: int = index, command: str = command,
        ) -> tuple[ExecCommandResult, RuntimeCallMetric]:
            return await timed_call(
                f"scenario1_mixed_c{concurrency:02d}_{index:02d}",
                handle.tool.shell(
                    command,
                    timeout=60,
                    description=f"scenario1 mixed-op c={concurrency} index={index}",
                ),
            )

        factories.append(run_worker)

    batch_start = time.perf_counter()
    rows = await gather_with_barrier(factories)
    batch_wall_ms = (time.perf_counter() - batch_start) * 1000.0

    metrics: list[RuntimeCallMetric] = []
    for index, (result, metric) in enumerate(rows):
        path = f"dist/scenario1/c{concurrency:02d}-{index:02d}.txt"
        assert_committed(result, path=path)
        assert result.exit_code == 0, result.stderr or result.stdout
        metrics.append(metric)
    return metrics, batch_wall_ms


async def test_mixed_op_concurrent_scaling_1_5_10(
    integrated_sandbox: SandboxHandle,
) -> None:
    handle = integrated_sandbox
    ignore = await handle.tool.write_file(
        ".gitignore", "dist/\n", description="seed gitignore for scenario1 scaling",
    )
    assert_committed(ignore, path=".gitignore")

    artifact = _artifact_path()
    if artifact.exists():
        artifact.unlink()

    before = await _probe_resources(handle)
    stream_row(artifact, {"schema": "scenario1.mixed_op_scaling.v1", "phase": "before", **before})

    baseline_p99: float | None = None
    summaries: list[dict[str, object]] = []
    failures: list[str] = []

    for concurrency in _CONCURRENCY_LEVELS:
        metrics, batch_wall_ms = await _run_mixed_level(handle, concurrency)
        per_call = [m.elapsed_ms for m in metrics]
        p99 = percentile(per_call, 99)
        if concurrency == 1:
            baseline_p99 = p99
        assert baseline_p99 is not None

        sampled = await _probe_resources(handle)
        row: dict[str, object] = {
            "schema": "scenario1.mixed_op_scaling.v1",
            "concurrency": concurrency,
            "calls": len(metrics),
            "batch_wall_ms": round(batch_wall_ms, 3),
            "per_call_p50_ms": round(percentile(per_call, 50), 3),
            "per_call_p99_ms": round(p99, 3),
            "per_call_max_ms": round(max(per_call), 3),
            "throughput_ops_s": round(concurrency / (batch_wall_ms / 1000.0), 3),
            **{f"res_{k}": v for k, v in sampled.items()},
        }
        stream_row(artifact, row)
        summaries.append(row)

        latency_ceiling = max(_LATENCY_RATIO * baseline_p99, _LATENCY_FLOOR_MS)
        if p99 > latency_ceiling:
            failures.append(
                f"c={concurrency}: per-call p99 {p99:.1f}ms > ceiling "
                f"{latency_ceiling:.1f}ms (baseline_c1 {baseline_p99:.1f}ms)"
            )
        if sampled.get("shm_count", 0) > _DEV_SHM_RUN_DIR_CEILING:
            failures.append(
                f"c={concurrency}: /dev/shm run-dirs {sampled['shm_count']} > "
                f"{_DEV_SHM_RUN_DIR_CEILING} (run-dir leak)"
            )
        if sampled.get("shm_bytes", 0) > _DEV_SHM_BYTES_CEILING:
            failures.append(
                f"c={concurrency}: /dev/shm bytes {sampled['shm_bytes']} > "
                f"{_DEV_SHM_BYTES_CEILING}"
            )

    after = await _probe_resources(handle)
    stream_row(artifact, {"schema": "scenario1.mixed_op_scaling.v1", "phase": "after", **after})

    rss_delta = after.get("hwm", 0) - before.get("hwm", 0)
    fd_delta = after.get("fds", 0) - before.get("fds", 0)
    if rss_delta > _RSS_DELTA_CEILING_KB:
        failures.append(f"daemon RSS HWM grew {rss_delta} KiB > {_RSS_DELTA_CEILING_KB} (leak)")
    if fd_delta > _FD_DELTA_CEILING:
        failures.append(f"daemon fd count grew {fd_delta} > {_FD_DELTA_CEILING} (leak)")

    emit_metric(
        "scenario1.mixed_op_concurrent_scaling",
        {"levels": summaries, "before": before, "after": after, "artifact": str(artifact)},
    )
    print(f"\n[scenario1:mixed_op_scaling] artifact={artifact}")
    assert not failures, "scenario1 scaling regression:\n" + "\n".join(failures)
