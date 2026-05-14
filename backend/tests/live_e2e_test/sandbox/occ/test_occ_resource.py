"""Phase 4 native resource-budget probes for OCC commits."""

from __future__ import annotations

import pytest

from .._harness.native_cases import run_native_case
from .._harness.sandbox_fixture import SandboxHandle


pytestmark = pytest.mark.asyncio


_BODY = r"""
from sandbox.layer_stack.manager import LayerStackManager
from sandbox.occ.changeset.types import WriteChange
from sandbox.occ.service import Service

class _Gitignore:
    def is_ignored(self, path):
        return path.startswith("dist/")

label = "occ.resource"
before = sample_resource()
started = time.perf_counter()
root = _case_root(label)
stack = LayerStackManager(root / "stack")
service = Service(gitignore=_Gitignore(), snapshot_reader=stack, staging=stack, publisher=stack)
latencies = []
timing_rows = []
for batch in range(12):
    changes = [
        WriteChange(path="tracked/%02d-%02d.txt" % (batch, index), final_content="tracked\n")
        for index in range(3)
    ]
    changes.extend(
        WriteChange(path="dist/%02d-%02d.txt" % (batch, index), final_content="ignored\n")
        for index in range(2)
    )
    t0 = time.perf_counter()
    result = service.apply_changeset_sync(changes)
    latencies.append((time.perf_counter() - t0) * 1000.0)
    timing_rows.append(result.timings)
    assert result.success

def timing_p99(key):
    return _percentile([row.get(key, 0.0) * 1000.0 for row in timing_rows], 99)

_emit(label, started, before, {
    "batches": len(latencies),
    "p50_ms": _percentile(latencies, 50),
    "p99_ms": _percentile(latencies, 99),
    "max_ms": max(latencies),
    "manifest_depth": stack.read_active_manifest().depth,
    "occ_apply_p99_ms": timing_p99("occ.apply.total_s"),
    "occ_prepare_p99_ms": timing_p99("occ.prepare.total_s"),
    "occ_commit_p99_ms": timing_p99("occ.commit.total_s"),
    "occ_serial_queue_p99_ms": timing_p99("occ.serial.queue_wait_s"),
    "layer_lock_wait_p99_ms": timing_p99("layer_stack.transaction.lock_wait_s"),
})
"""


async def test_occ_resource_budgets_hold(native_sandbox: SandboxHandle) -> None:
    payload = await run_native_case(
        native_sandbox,
        _BODY,
        label="occ.resource",
    )
    assert payload["batches"] == 12
    assert payload["manifest_depth"] == 12
