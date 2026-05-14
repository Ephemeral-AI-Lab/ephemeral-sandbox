"""Phase 4 native load probe for layer-stack publish throughput."""

from __future__ import annotations

import pytest

from .._harness.load_profiles import LAYER_STACK_LOAD
from .._harness.native_cases import run_native_case
from .._harness.sandbox_fixture import SandboxHandle


pytestmark = pytest.mark.asyncio


_BODY = r"""
from sandbox.layer_stack.layer_change import LayerChange, WriteLayerChange
from sandbox.layer_stack.manager import LayerStackManager

cfg = json.loads(__CFG_JSON__)
label = "layer_stack.load"
before = sample_resource()
started = time.perf_counter()
root = _case_root(label)
manager = LayerStackManager(root / "stack")
operation_count = int(cfg["operation_count"])
concurrency = int(cfg["concurrency"])
barrier = threading.Barrier(concurrency)

def publish_one(index):
    source = _source(root, "load-%04d" % index, ("payload-%04d\n" % index).encode("utf-8"))
    change = WriteLayerChange(path="load/%04d.txt" % index, source_path=str(source))
    barrier.wait(timeout=10)
    timings = {}
    t0 = time.perf_counter()
    with manager.commit_transaction() as transaction:
        manifest = transaction.publish_layer([change], timings=timings)
    elapsed_ms = (time.perf_counter() - t0) * 1000.0
    return {
        "index": index,
        "version": manifest.version,
        "elapsed_ms": elapsed_ms,
        "lock_wait_ms": timings.get("layer_stack.transaction.lock_wait_s", 0.0) * 1000.0,
        "publish_ms": timings.get("layer_stack.publish.total_s", 0.0) * 1000.0,
    }

with concurrent.futures.ThreadPoolExecutor(max_workers=concurrency) as pool:
    rows = list(pool.map(publish_one, range(operation_count)))

for index in range(operation_count):
    assert manager.read_text("load/%04d.txt" % index) == ("payload-%04d\n" % index, True)
manifest = manager.read_active_manifest()
assert manifest.depth == operation_count
latencies = [row["elapsed_ms"] for row in rows]
publish_latencies = [row["publish_ms"] for row in rows]
lock_waits = [row["lock_wait_ms"] for row in rows]
before_squash_depth = manifest.depth
squash_t0 = time.perf_counter()
squashed = manager.squash(max_depth=20)
squash_ms = (time.perf_counter() - squash_t0) * 1000.0
after_squash_depth = manager.read_active_manifest().depth
assert squashed is not None
assert after_squash_depth <= 20
coalesced = before_squash_depth - after_squash_depth
coalesce_layers_per_s = coalesced / max(squash_ms / 1000.0, 0.001)

_emit(label, started, before, {
    "profile": cfg,
    "operations": len(rows),
    "manifest_depth_before_squash": before_squash_depth,
    "manifest_depth_after_squash": after_squash_depth,
    "append_p50_ms": _percentile(latencies, 50),
    "append_p99_ms": _percentile(latencies, 99),
    "append_max_ms": max(latencies),
    "publish_p50_ms": _percentile(publish_latencies, 50),
    "publish_p99_ms": _percentile(publish_latencies, 99),
    "publish_max_ms": max(publish_latencies),
    "lock_wait_p99_ms": _percentile(lock_waits, 99),
    "squash_ms": squash_ms,
    "squash_coalesce_layers_per_s": coalesce_layers_per_s,
})
"""


async def test_layer_stack_publish_load_and_squash_budget(
    native_sandbox: SandboxHandle,
) -> None:
    payload = await run_native_case(
        native_sandbox,
        _BODY,
        label="layer_stack.load",
        cfg={
            "operation_count": LAYER_STACK_LOAD.operation_count,
            "concurrency": LAYER_STACK_LOAD.concurrency,
            "max_p99_ms": LAYER_STACK_LOAD.max_p99_ms,
        },
        timeout=240,
    )
    assert payload["operations"] == LAYER_STACK_LOAD.operation_count
    assert payload["publish_p99_ms"] < LAYER_STACK_LOAD.max_p99_ms
    assert payload["manifest_depth_after_squash"] <= 20
