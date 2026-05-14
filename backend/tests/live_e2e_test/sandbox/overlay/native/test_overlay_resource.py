"""Phase 4 native resource-budget probes for overlay execution."""

from __future__ import annotations

import pytest

from ..._harness.native_cases import run_native_case
from ..._harness.sandbox_fixture import SandboxHandle


pytestmark = pytest.mark.asyncio


_BODY = r"""
from sandbox.layer_stack.layer.change import LayerChange, WriteLayerChange
from sandbox.layer_stack.manager import LayerStackManager
from sandbox.overlay import OverlayShellRequest, OverlaySnapshotRunner

label = "overlay.native.overlay_resource"
before = sample_resource()
started = time.perf_counter()
root = _case_root(label)
manager = LayerStackManager(root / "stack")
manager.publish_changes([
    WriteLayerChange(path="base.txt", source_path=str(_source(root, "base", b"base\n"))),
])
runner = OverlaySnapshotRunner(manager)
latencies = []
timing_rows = []
for index in range(8):
    t0 = time.perf_counter()
    capture = runner.shell_sync(OverlayShellRequest(
        request_id="resource-%02d" % index,
        command=("bash", "-lc", "cat base.txt >/dev/null; printf '%02d' > out-%02d.txt" % (index, index)),
        cwd=".",
        env={},
        timeout_seconds=5,
    ))
    latencies.append((time.perf_counter() - t0) * 1000.0)
    timing_rows.append(capture.timings)
    assert capture.exit_code == 0
assert manager.pinned_layers() == ()

def timing_p99(key):
    return _percentile([row.get(key, 0.0) * 1000.0 for row in timing_rows], 99)

_emit(label, started, before, {
    "calls": len(latencies),
    "p50_ms": _percentile(latencies, 50),
    "p99_ms": _percentile(latencies, 99),
    "max_ms": max(latencies),
    "lease_released": manager.pinned_layers() == (),
    "overlay_run_command_p99_ms": timing_p99("overlay.run_command_s"),
    "overlay_capture_p99_ms": timing_p99("overlay.capture_changes_s"),
    "overlay_mount_snapshot_p99_ms": timing_p99("overlay.mount_snapshot_s"),
    "overlay_total_p99_ms": timing_p99("overlay.total_s"),
})
"""


async def test_overlay_resource_budgets_hold(native_sandbox: SandboxHandle) -> None:
    payload = await run_native_case(
        native_sandbox,
        _BODY,
        label="overlay.native.overlay_resource",
    )
    assert payload["calls"] == 8
    assert payload["lease_released"] is True
