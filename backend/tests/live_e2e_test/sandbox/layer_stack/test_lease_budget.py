"""Phase 2 native probes for layer-stack lease-budget decisions."""

from __future__ import annotations

import pytest

from .._harness.native_cases import run_native_case
from .._harness.sandbox_fixture import SandboxHandle


pytestmark = pytest.mark.asyncio


_BUDGET_BODY = r"""
from sandbox.layer_stack.changes import LayerChange
from sandbox.layer_stack.lease_budget import LeaseBudgetWorker
from sandbox.layer_stack.publisher import CommitBackpressureError
from sandbox.layer_stack.stack_manager import LayerStackManager

label = "layer_stack.lease_budget"
before = sample_resource()
started = time.perf_counter()
root = _case_root(label)

zero = LeaseBudgetWorker(max_active_depth=0).evaluate(active_depth=0, snapshots=[])
one = LeaseBudgetWorker(max_active_depth=1)
assert zero.kind == "backpressure_commits"
assert one.evaluate(active_depth=0, snapshots=[]).kind == "allow"
assert one.evaluate(active_depth=1, snapshots=[]).kind == "backpressure_commits"
infinite = LeaseBudgetWorker(max_active_depth=None).evaluate(active_depth=10_000, snapshots=[])
assert infinite.kind == "allow"

manager = LayerStackManager(root / "stack")
manager.publish_changes([
    LayerChange(path="payload.txt", kind="write", source_path=str(_source(root, "payload", b"bytes"))),
])
lease = manager.acquire_snapshot_lease("request-a")
pinned_worker = LeaseBudgetWorker(max_pinned_bytes=5)
blocked = pinned_worker.evaluate(active_depth=manager.read_active_manifest().depth, snapshots=manager.lease_snapshots())
assert blocked.kind == "backpressure_commits"
manager.release_lease(lease.lease_id)
refreshed = pinned_worker.evaluate(active_depth=manager.read_active_manifest().depth, snapshots=manager.lease_snapshots())
assert refreshed.kind == "allow"

_emit(label, started, before, {
    "budget_zero": zero.kind,
    "budget_one_before": one.evaluate(active_depth=0, snapshots=[]).kind,
    "budget_one_at_boundary": one.evaluate(active_depth=1, snapshots=[]).kind,
    "budget_infinite": infinite.kind,
    "pinned_blocked": blocked.kind,
    "after_release_refresh": refreshed.kind,
})
"""


_RACE_BODY = r"""
from sandbox.layer_stack.changes import LayerChange
from sandbox.layer_stack.lease_budget import LeaseBudgetWorker
from sandbox.layer_stack.publisher import CommitBackpressureError
from sandbox.layer_stack.stack_manager import LayerStackManager

label = "layer_stack.lease_budget_under_race"
before = sample_resource()
started = time.perf_counter()
root = _case_root(label)
b = 4
n = b + 4
manager = LayerStackManager(root / "stack", lease_budget=LeaseBudgetWorker(max_active_depth=b))
barrier = threading.Barrier(n)

def publish_one(index):
    barrier.wait(timeout=5)
    try:
        manifest = manager.publish_changes([
            LayerChange(
                path="race/%02d.txt" % index,
                kind="write",
                source_path=str(_source(root, "race-%02d" % index, ("value-%02d\n" % index).encode("utf-8"))),
            )
        ])
    except CommitBackpressureError:
        return {"index": index, "status": "rejected", "version": None}
    return {"index": index, "status": "accepted", "version": manifest.version}

with concurrent.futures.ThreadPoolExecutor(max_workers=n) as pool:
    rows = list(pool.map(publish_one, range(n)))

accepted = [row for row in rows if row["status"] == "accepted"]
rejected = [row for row in rows if row["status"] == "rejected"]
manifest = manager.read_active_manifest()
assert len(accepted) == b, rows
assert len(rejected) == n - b, rows
assert manifest.depth == b, manifest
for row in accepted:
    assert manager.read_text("race/%02d.txt" % row["index"])[1] is True
for row in rejected:
    assert manager.read_text("race/%02d.txt" % row["index"]) == ("", False)

_emit(label, started, before, {
    "budget": b,
    "attempted_grabs": n,
    "accepted": len(accepted),
    "rejected": len(rejected),
    "manifest_depth": manifest.depth,
    "accepted_versions": sorted(row["version"] for row in accepted),
})
"""


async def test_lease_budget_handles_zero_one_infinite_and_refresh(
    native_sandbox: SandboxHandle,
) -> None:
    payload = await run_native_case(
        native_sandbox,
        _BUDGET_BODY,
        label="layer_stack.lease_budget",
    )
    assert payload["budget_zero"] == "backpressure_commits"
    assert payload["budget_one_before"] == "allow"
    assert payload["budget_one_at_boundary"] == "backpressure_commits"
    assert payload["budget_infinite"] == "allow"
    assert payload["after_release_refresh"] == "allow"


async def test_lease_budget_under_race_accepts_exactly_boundary_budget(
    native_sandbox: SandboxHandle,
) -> None:
    payload = await run_native_case(
        native_sandbox,
        _RACE_BODY,
        label="layer_stack.lease_budget_under_race",
    )
    assert payload["accepted"] == payload["budget"]
    assert payload["rejected"] == 4
    assert payload["manifest_depth"] == payload["budget"]
