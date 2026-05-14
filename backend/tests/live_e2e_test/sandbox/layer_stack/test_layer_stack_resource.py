"""Phase 4 native resource-budget probes for layer-stack reads and publishes."""

from __future__ import annotations

import pytest

from .._harness.native_cases import run_native_case
from .._harness.sandbox_fixture import SandboxHandle


pytestmark = pytest.mark.asyncio


_BODY = r"""
from sandbox.layer_stack.layer_change import LayerChange, WriteLayerChange
from sandbox.layer_stack.manager import LayerStackManager

label = "layer_stack.resource"
before = sample_resource()
started = time.perf_counter()
root = _case_root(label)
manager = LayerStackManager(root / "stack")
latencies = []
depths = (100, 200)
depth_metrics = {}

for depth in range(1, max(depths) + 1):
    source = _source(root, "resource-%03d" % depth, ("value-%03d\n" % depth).encode("utf-8"))
    t0 = time.perf_counter()
    manager.publish_changes([
        WriteLayerChange(path="values/%03d.txt" % depth, source_path=str(source)),
    ])
    latencies.append((time.perf_counter() - t0) * 1000.0)
    if depth in depths:
        read_t0 = time.perf_counter()
        listing = manager.list_dir("values")
        read_ms = (time.perf_counter() - read_t0) * 1000.0
        assert len(listing) == depth
        assert manager.read_text("values/%03d.txt" % depth) == ("value-%03d\n" % depth, True)
        depth_metrics[str(depth)] = {
            "read_ms": read_ms,
            "manifest_depth": manager.read_active_manifest().depth,
            "resource_mid": sample_resource(),
        }

assert manager.read_active_manifest().depth == max(depths)

_emit(label, started, before, {
    "depth_metrics": depth_metrics,
    "publish_p50_ms": _percentile(latencies, 50),
    "publish_p99_ms": _percentile(latencies, 99),
    "publish_max_ms": max(latencies),
})
"""


async def test_layer_stack_depth_100_and_200_resource_budgets(
    native_sandbox: SandboxHandle,
) -> None:
    payload = await run_native_case(
        native_sandbox,
        _BODY,
        label="layer_stack.resource",
        timeout=180,
    )
    assert payload["depth_metrics"]["100"]["manifest_depth"] == 100
    assert payload["depth_metrics"]["200"]["manifest_depth"] == 200
