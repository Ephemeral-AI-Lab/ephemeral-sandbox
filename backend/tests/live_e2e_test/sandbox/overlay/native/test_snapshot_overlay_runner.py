"""Phase 1b native probes for snapshot overlay runner."""

from __future__ import annotations

import pytest

from ..._harness.native_cases import run_native_case
from ..._harness.sandbox_fixture import SandboxHandle


pytestmark = pytest.mark.asyncio


_RUNNER_BODY = r"""
from sandbox.layer_stack.changes import LayerChange
from sandbox.layer_stack.stack_manager import LayerStackManager
from sandbox.overlay.capture.types import read_output_ref
from sandbox.overlay.runner.snapshot_overlay_runner import OverlayShellRequest, SnapshotOverlayRunner

label = "overlay.native.snapshot_overlay_runner"
before = sample_resource()
started = time.perf_counter()
root = _case_root(label)
manager = LayerStackManager(root / "stack")
manager.publish_changes([
    LayerChange(path="pkg/value.txt", kind="write", source_path=str(_source(root, "value", b"old\n"))),
])
runner = SnapshotOverlayRunner(manager)
request = OverlayShellRequest(
    request_id="request-a",
    command=("bash", "-lc", "mkdir -p nested/dir; printf new > pkg/value.txt; printf nested > nested/dir/out.txt; printf ok"),
    cwd=".",
    env={},
    timeout_seconds=5,
)
capture = runner.shell_sync(request)
changes = {change.path: change.kind for change in capture.changes}
assert capture.exit_code == 0
assert read_output_ref(capture.stdout_ref) == "ok"
assert manager.read_text("pkg/value.txt") == ("old\n", True)
assert changes["pkg/value.txt"] == "write"
assert changes["nested/dir/out.txt"] == "write"
assert manager.pinned_layers() == ()

class _FailingInvoker:
    def invoke_sync(self, **_kwargs):
        raise RuntimeError("runtime failed")

failing_runner = SnapshotOverlayRunner(manager, invoker=_FailingInvoker())
try:
    failing_runner.shell_sync(OverlayShellRequest(
        request_id="request-fails",
        command=("bash", "-lc", "true"),
        cwd=".",
        env={},
        timeout_seconds=5,
    ))
except RuntimeError:
    pass
else:
    raise AssertionError("failing invoker did not raise")
assert manager.pinned_layers() == ()

_emit(label, started, before, {
    "exit_code": capture.exit_code,
    "changes": changes,
    "lease_released_after_success": manager.pinned_layers() == (),
    "lease_released_after_failure": True,
    "timings": capture.timings,
})
"""


_RACE_BODY = r"""
from sandbox.layer_stack.changes import LayerChange
from sandbox.layer_stack.stack_manager import LayerStackManager
from sandbox.overlay.runner.snapshot_overlay_runner import OverlayShellRequest, SnapshotOverlayRunner

label = "overlay.native.snapshot_overlay_runner_under_race"
before = sample_resource()
started = time.perf_counter()
root = _case_root(label)
manager = LayerStackManager(root / "stack")
manager.publish_changes([
    LayerChange(path="base.txt", kind="write", source_path=str(_source(root, "base", b"base\n"))),
])
runner = SnapshotOverlayRunner(manager)
n = 4
barrier = threading.Barrier(n)

def run_one(index):
    barrier.wait(timeout=5)
    request = OverlayShellRequest(
        request_id="race-%02d" % index,
        command=("bash", "-lc", "printf value-%02d > out-%02d.txt; cat base.txt >/dev/null" % (index, index)),
        cwd=".",
        env={},
        timeout_seconds=5,
    )
    t0 = time.perf_counter()
    capture = runner.shell_sync(request)
    return {
        "index": index,
        "exit_code": capture.exit_code,
        "paths": sorted(change.path for change in capture.changes),
        "elapsed_ms": (time.perf_counter() - t0) * 1000.0,
    }

with concurrent.futures.ThreadPoolExecutor(max_workers=n) as pool:
    rows = list(pool.map(run_one, range(n)))
assert all(row["exit_code"] == 0 for row in rows), rows
for index, row in enumerate(sorted(rows, key=lambda item: item["index"])):
    assert row["paths"] == ["out-%02d.txt" % index], row
assert manager.pinned_layers() == ()
latencies = [row["elapsed_ms"] for row in rows]

_emit(label, started, before, {
    "runners": n,
    "captured_paths": [row["paths"] for row in sorted(rows, key=lambda item: item["index"])],
    "lease_released": manager.pinned_layers() == (),
    "runner_p50_ms": _percentile(latencies, 50),
    "runner_p99_ms": _percentile(latencies, 99),
    "runner_max_ms": max(latencies),
})
"""


async def test_snapshot_overlay_runner_round_trips_and_releases_leases(
    native_sandbox: SandboxHandle,
) -> None:
    payload = await run_native_case(
        native_sandbox,
        _RUNNER_BODY,
        label="overlay.native.snapshot_overlay_runner",
    )
    assert payload["exit_code"] == 0
    assert payload["lease_released_after_success"] is True
    assert payload["lease_released_after_failure"] is True


async def test_snapshot_overlay_runner_under_race_has_no_cross_leak(
    native_sandbox: SandboxHandle,
) -> None:
    payload = await run_native_case(
        native_sandbox,
        _RACE_BODY,
        label="overlay.native.snapshot_overlay_runner_under_race",
    )
    assert payload["runners"] == 4
    assert payload["lease_released"] is True
