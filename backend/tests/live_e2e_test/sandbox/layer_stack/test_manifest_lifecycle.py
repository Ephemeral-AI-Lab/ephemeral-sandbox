"""Phase 1a native probes for layer-stack manifest lifecycle."""

from __future__ import annotations

import pytest

from .._harness.native_cases import run_native_case
from .._harness.sandbox_fixture import SandboxHandle


pytestmark = pytest.mark.asyncio


_LIFECYCLE_BODY = r"""
from sandbox.layer_stack.changes import LayerChange
from sandbox.layer_stack.manifest import LayerRef, Manifest, read_manifest, write_manifest_atomic
from sandbox.layer_stack.stack_manager import LayerStackManager

label = "layer_stack.manifest_lifecycle"
before = sample_resource()
started = time.perf_counter()
root = _case_root(label)

manifest_file = root / "manifest.json"
assert read_manifest(manifest_file).version == 0
manifest = Manifest(
    version=2,
    layers=(
        LayerRef(layer_id="L000002", path="layers/L000002"),
        LayerRef(layer_id="L000001", path="layers/L000001"),
    ),
)
write_manifest_atomic(manifest_file, manifest)
assert read_manifest(manifest_file) == manifest

manager = LayerStackManager(root / "stack")
first = manager.publish_changes([
    LayerChange(path="pkg/value.txt", kind="write", source_path=str(_source(root, "value-1", b"one"))),
])
lease = manager.acquire_snapshot_lease("request-a")
second = manager.publish_changes([
    LayerChange(path="pkg/value.txt", kind="write", source_path=str(_source(root, "value-2", b"two"))),
])
restarted = LayerStackManager(root / "stack")
assert restarted.read_active_manifest() == second
assert restarted.read_text("pkg/value.txt") == ("two", True)
assert restarted.read_text("pkg/value.txt", manifest=lease.manifest) == ("one", True)
assert restarted.release_lease(lease.lease_id) is False
assert manager.release_lease(lease.lease_id) is True

corrupt = root / "corrupt.json"
corrupt.write_text("{not-json", encoding="utf-8")
corrupt_detected = False
try:
    read_manifest(corrupt)
except Exception:
    corrupt_detected = True
assert corrupt_detected

_emit(label, started, before, {
    "manifest_depth": second.depth,
    "manifest_version": second.version,
    "round_trip_layers": [layer.layer_id for layer in read_manifest(manifest_file).layers],
    "corrupt_detected": corrupt_detected,
})
"""


_RACE_BODY = r"""
from sandbox.layer_stack.changes import LayerChange
from sandbox.layer_stack.stack_manager import LayerStackManager

label = "layer_stack.manifest_lifecycle_under_race"
before = sample_resource()
started = time.perf_counter()
root = _case_root(label)
manager = LayerStackManager(root / "stack")
n = 8
barrier = threading.Barrier(n)
latencies = []

def append_one(index):
    source = _source(root, "race-%02d" % index, ("value-%02d\n" % index).encode("utf-8"))
    change = LayerChange(
        path="race/%02d.txt" % index,
        kind="write",
        content_hash=_sha(("value-%02d\n" % index).encode("utf-8")),
        source_path=str(source),
    )
    barrier.wait(timeout=5)
    t0 = time.perf_counter()
    manifest = manager.publish_changes([change])
    elapsed = (time.perf_counter() - t0) * 1000.0
    return {"index": index, "version": manifest.version, "elapsed_ms": elapsed}

with concurrent.futures.ThreadPoolExecutor(max_workers=n) as pool:
    results = list(pool.map(append_one, range(n)))

manifest = manager.read_active_manifest()
assert manifest.depth == n, manifest
for index in range(n):
    expected = ("value-%02d\n" % index).encode("utf-8")
    assert manager.read_bytes("race/%02d.txt" % index) == (expected, True)
latencies = [row["elapsed_ms"] for row in results]
assert len({row["version"] for row in results}) == n

_emit(label, started, before, {
    "appenders": n,
    "manifest_depth": manifest.depth,
    "versions": sorted(row["version"] for row in results),
    "append_p50_ms": _percentile(latencies, 50),
    "append_p99_ms": _percentile(latencies, 99),
    "append_max_ms": max(latencies),
})
"""


async def test_manifest_lifecycle_round_trips_and_detects_corruption(
    native_sandbox: SandboxHandle,
) -> None:
    payload = await run_native_case(
        native_sandbox,
        _LIFECYCLE_BODY,
        label="layer_stack.manifest_lifecycle",
    )
    assert payload["manifest_depth"] == 2
    assert payload["corrupt_detected"] is True


async def test_manifest_append_under_race_has_no_torn_entries(
    native_sandbox: SandboxHandle,
) -> None:
    payload = await run_native_case(
        native_sandbox,
        _RACE_BODY,
        label="layer_stack.manifest_lifecycle_under_race",
    )
    assert payload["appenders"] == 8
    assert payload["manifest_depth"] == 8
