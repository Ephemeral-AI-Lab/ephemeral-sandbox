"""Phase 1a native probes for immutable layer publishing."""

from __future__ import annotations

import pytest

from .._harness.native_cases import run_native_case
from .._harness.sandbox_fixture import SandboxHandle


pytestmark = pytest.mark.asyncio


_PUBLISHER_BODY = r"""
from sandbox.layer_stack.layer_change import LayerChange, WriteLayerChange
from sandbox.layer_stack.manifest import Manifest
from sandbox.layer_stack.manager import LayerStackManager

label = "layer_stack.publisher"
before = sample_resource()
started = time.perf_counter()
root = _case_root(label)
manager = LayerStackManager(root / "stack")

payload = b"payload\n"
change = WriteLayerChange(
             path="pkg/published.txt",
             content_hash=_sha(payload),
             source_path=str(_source(root, "payload", payload)),
         )
first = manager.publish_changes([change])
same = manager.publish_changes([change])
assert same == first
assert manager.read_bytes("pkg/published.txt") == (payload, True)

bad_source = _source(root, "bad", b"actual")
try:
    manager.publish_changes([
        WriteLayerChange(
            path="pkg/bad.txt",
            content_hash=_sha(b"expected"),
            source_path=str(bad_source),
        )
    ])
except ValueError:
    pass
else:
    raise AssertionError("content-hash mismatch did not abort")

active = manager.read_active_manifest()
staging_entries = sorted(p.name for p in (manager.storage_root / "staging").iterdir())
layer_entries = sorted(p.name for p in (manager.storage_root / "layers").iterdir())
assert active == first
assert len(layer_entries) == 1
assert staging_entries == []

_emit(label, started, before, {
    "manifest_version": active.version,
    "manifest_depth": active.depth,
    "idempotent_same_digest": same == first,
    "layer_entries": layer_entries,
    "staging_entries": staging_entries,
})
"""


_RACE_BODY = r"""
from sandbox.layer_stack.layer_change import LayerChange, WriteLayerChange
from sandbox.layer_stack.manager import LayerStackManager

label = "layer_stack.publisher_under_race"
before = sample_resource()
started = time.perf_counter()
root = _case_root(label)
manager = LayerStackManager(root / "stack")
n = 8
barrier = threading.Barrier(n)
payload = b"same-digest\n"
latencies = []

def publish_same(index):
    source = _source(root, "same-%02d" % index, payload)
    change = WriteLayerChange(
                 path="pkg/same.txt",
                 content_hash=_sha(payload),
                 source_path=str(source),
             )
    barrier.wait(timeout=5)
    t0 = time.perf_counter()
    manifest = manager.publish_changes([change])
    return {"index": index, "version": manifest.version, "elapsed_ms": (time.perf_counter() - t0) * 1000.0}

with concurrent.futures.ThreadPoolExecutor(max_workers=n) as pool:
    results = list(pool.map(publish_same, range(n)))

manifest = manager.read_active_manifest()
layer_entries = sorted(p.name for p in (manager.storage_root / "layers").iterdir())
assert manifest.version == 1, manifest
assert manifest.depth == 1, manifest
assert len(layer_entries) == 1, layer_entries
assert manager.read_bytes("pkg/same.txt") == (payload, True)
latencies = [row["elapsed_ms"] for row in results]
already_published = sum(1 for row in results if row["version"] == 1) - 1

_emit(label, started, before, {
    "publishers": n,
    "manifest_version": manifest.version,
    "manifest_depth": manifest.depth,
    "canonical_refs": len(layer_entries),
    "already_published": already_published,
    "publish_p50_ms": _percentile(latencies, 50),
    "publish_p99_ms": _percentile(latencies, 99),
    "publish_max_ms": max(latencies),
})
"""


async def test_publisher_is_atomic_and_idempotent_for_same_digest(
    native_sandbox: SandboxHandle,
) -> None:
    payload = await run_native_case(
        native_sandbox,
        _PUBLISHER_BODY,
        label="layer_stack.publisher",
    )
    assert payload["idempotent_same_digest"] is True
    assert payload["manifest_depth"] == 1
    assert payload["staging_entries"] == []


async def test_publisher_under_race_keeps_one_canonical_ref(
    native_sandbox: SandboxHandle,
) -> None:
    payload = await run_native_case(
        native_sandbox,
        _RACE_BODY,
        label="layer_stack.publisher_under_race",
    )
    assert payload["publishers"] == 8
    assert payload["canonical_refs"] == 1
    assert payload["already_published"] == 7
