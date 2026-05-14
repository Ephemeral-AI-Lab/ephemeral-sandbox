"""Phase 4 native load probe for snapshot overlay runner."""

from __future__ import annotations

import pytest

from ..._harness.load_profiles import OVERLAY_RUNNER_LOAD
from ..._harness.native_cases import run_native_case
from ..._harness.sandbox_fixture import SandboxHandle


pytestmark = pytest.mark.asyncio


_BODY = r"""
from sandbox.layer_stack.layer.change import LayerChange, WriteLayerChange
from sandbox.layer_stack.manager import LayerStackManager
from sandbox.overlay import OverlayShellRequest, OverlaySnapshotRunner

cfg = json.loads(__CFG_JSON__)
label = "overlay.native.overlay_runner_load"
before = sample_resource()
started = time.perf_counter()
root = _case_root(label)
manager = LayerStackManager(root / "stack")
manager.publish_changes([
    WriteLayerChange(path="base.txt", source_path=str(_source(root, "base", b"base\n"))),
])
runner = OverlaySnapshotRunner(manager)
operation_count = int(cfg["operation_count"])
concurrency = int(cfg["concurrency"])
barrier = threading.Barrier(concurrency)

def run_one(index):
    barrier.wait(timeout=10)
    t0 = time.perf_counter()
    capture = runner.shell_sync(OverlayShellRequest(
        request_id="load-%02d" % index,
        command=("bash", "-lc", "cat base.txt >/dev/null; printf load-%02d > load-%02d.txt" % (index, index)),
        cwd=".",
        env={},
        timeout_seconds=10,
    ))
    return {
        "index": index,
        "exit_code": capture.exit_code,
        "paths": sorted(change.path for change in capture.changes),
        "elapsed_ms": (time.perf_counter() - t0) * 1000.0,
        "timings": capture.timings,
    }

with concurrent.futures.ThreadPoolExecutor(max_workers=concurrency) as pool:
    rows = list(pool.map(run_one, range(operation_count)))
assert all(row["exit_code"] == 0 for row in rows), rows
assert manager.pinned_layers() == ()
latencies = [row["elapsed_ms"] for row in rows]
p99 = _percentile(latencies, 99)

def timing_p99(key):
    return _percentile([row["timings"].get(key, 0.0) * 1000.0 for row in rows], 99)

_emit(label, started, before, {
    "profile": cfg,
    "calls": len(rows),
    "p50_ms": _percentile(latencies, 50),
    "p99_ms": p99,
    "max_ms": max(latencies),
    "budget_ms": float(cfg["max_p99_ms"]),
    "budget_met": p99 < float(cfg["max_p99_ms"]),
    "lease_released": manager.pinned_layers() == (),
    "overlay_run_command_p99_ms": timing_p99("overlay.run_command_s"),
    "overlay_capture_p99_ms": timing_p99("overlay.capture_changes_s"),
    "overlay_mount_snapshot_p99_ms": timing_p99("overlay.mount_snapshot_s"),
    "overlay_total_p99_ms": timing_p99("overlay.total_s"),
})
"""


async def test_overlay_runner_load_has_no_fd_or_mount_leaks(
    native_sandbox: SandboxHandle,
) -> None:
    payload = await run_native_case(
        native_sandbox,
        _BODY,
        label="overlay.native.overlay_runner_load",
        cfg={
            "operation_count": OVERLAY_RUNNER_LOAD.operation_count,
            "concurrency": OVERLAY_RUNNER_LOAD.concurrency,
            "max_p99_ms": OVERLAY_RUNNER_LOAD.max_p99_ms,
        },
        timeout=180,
    )
    assert payload["calls"] == OVERLAY_RUNNER_LOAD.operation_count
    assert payload["p99_ms"] < 5_000
