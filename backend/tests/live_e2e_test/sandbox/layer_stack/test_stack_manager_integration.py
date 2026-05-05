"""Phase 2 native probes for the layer-stack manager facade."""

from __future__ import annotations

import pytest

from .._harness.native_cases import run_native_case
from .._harness.sandbox_fixture import SandboxHandle


pytestmark = pytest.mark.asyncio


_INTEGRATION_BODY = r"""
from sandbox.layer_stack.changes import LayerChange
from sandbox.layer_stack.lease_budget import LeaseBudgetWorker
from sandbox.layer_stack.publisher import CommitBackpressureError
from sandbox.layer_stack.stack_manager import LayerStackManager

label = "layer_stack.stack_manager_integration"
before = sample_resource()
started = time.perf_counter()
root = _case_root(label)
manager = LayerStackManager(root / "stack")

base = manager.publish_changes([
    LayerChange(path="src/app.py", kind="write", source_path=str(_source(root, "app-base", b"base\n"))),
])
lease = manager.acquire_snapshot_lease("agent-a")
updated = manager.publish_changes([
    LayerChange(path="src/app.py", kind="write", source_path=str(_source(root, "app-next", b"next\n"))),
    LayerChange(path="build/out.txt", kind="write", source_path=str(_source(root, "build-out", b"build\n"))),
])
assert manager.read_text("src/app.py") == ("next\n", True)
assert manager.read_text("src/app.py", manifest=lease.manifest) == ("base\n", True)
assert manager.list_dir("build") == ("out.txt",)

materialized = root / "materialized"
manager.materialize(materialized)
assert (materialized / "src" / "app.py").read_text(encoding="utf-8") == "next\n"

bad_hash_rejected = False
try:
    manager.publish_changes([
        LayerChange(
            path="src/bad.py",
            kind="write",
            content_hash=_sha(b"expected"),
            source_path=str(_source(root, "bad", b"actual")),
        )
    ])
except ValueError:
    bad_hash_rejected = True
assert bad_hash_rejected
assert manager.read_bytes("src/bad.py") == (None, False)

blocked_manager = LayerStackManager(
    root / "blocked-stack",
    lease_budget=LeaseBudgetWorker(max_active_depth=0),
)
backpressure_rejected = False
try:
    blocked_manager.publish_changes([
        LayerChange(path="blocked.txt", kind="write", source_path=str(_source(root, "blocked", b"blocked"))),
    ])
except CommitBackpressureError:
    backpressure_rejected = True
assert backpressure_rejected
assert list((blocked_manager.storage_root / "staging").iterdir()) == []

expired = manager.expire_leases_older_than(0, now=lease.acquired_at + 1)
assert expired == (lease,)
squashed = manager.squash(max_depth=1)
assert squashed is not None
cleanup = manager.collect_garbage(young_staging_age_seconds=0)
assert cleanup.missing_active_layers == ()
assert cleanup.missing_leased_layers == ()
assert manager.read_text("src/app.py") == ("next\n", True)
assert manager.read_text("build/out.txt") == ("build\n", True)

_emit(label, started, before, {
    "base_version": base.version,
    "updated_version": updated.version,
    "squashed_depth": squashed.depth,
    "bad_hash_rejected": bad_hash_rejected,
    "backpressure_rejected": backpressure_rejected,
    "expired_leases": [item.lease_id for item in expired],
    "gc_removed_layers": len(cleanup.orphan_layers_removed),
    "missing_active_layers": len(cleanup.missing_active_layers),
})
"""


_RACE_BODY = r"""
from sandbox.layer_stack.changes import LayerChange
from sandbox.layer_stack.stack_manager import LayerStackManager

label = "layer_stack.stack_manager_integration_under_race"
before = sample_resource()
started = time.perf_counter()
root = _case_root(label)
manager = LayerStackManager(root / "stack")
manager.publish_changes([
    LayerChange(path="shared/base.txt", kind="write", source_path=str(_source(root, "base", b"base\n"))),
])
n = 4
barrier = threading.Barrier(n)

def agent_flow(index):
    lease = manager.acquire_snapshot_lease("agent-%02d" % index)
    assert manager.read_text("shared/base.txt", manifest=lease.manifest) == ("base\n", True)
    barrier.wait(timeout=5)
    t0 = time.perf_counter()
    manifest = manager.publish_changes([
        LayerChange(
            path="agents/%02d.txt" % index,
            kind="write",
            source_path=str(_source(root, "agent-%02d" % index, ("agent-%02d\n" % index).encode("utf-8"))),
        )
    ])
    released = manager.release_lease(lease.lease_id)
    return {
        "index": index,
        "version": manifest.version,
        "released": released,
        "elapsed_ms": (time.perf_counter() - t0) * 1000.0,
    }

with concurrent.futures.ThreadPoolExecutor(max_workers=n) as pool:
    rows = list(pool.map(agent_flow, range(n)))

manifest = manager.read_active_manifest()
assert all(row["released"] for row in rows), rows
assert manager.pinned_layers() == ()
assert manifest.depth == n + 1, manifest
for index in range(n):
    assert manager.read_text("agents/%02d.txt" % index) == ("agent-%02d\n" % index, True)
assert manager.read_text("shared/base.txt") == ("base\n", True)
latencies = [row["elapsed_ms"] for row in rows]

_emit(label, started, before, {
    "agents": n,
    "manifest_depth": manifest.depth,
    "versions": sorted(row["version"] for row in rows),
    "all_leases_released": manager.pinned_layers() == (),
    "agent_p50_ms": _percentile(latencies, 50),
    "agent_p99_ms": _percentile(latencies, 99),
    "agent_max_ms": max(latencies),
})
"""


async def test_stack_manager_happy_path_and_failure_injection(
    native_sandbox: SandboxHandle,
) -> None:
    payload = await run_native_case(
        native_sandbox,
        _INTEGRATION_BODY,
        label="layer_stack.stack_manager_integration",
    )
    assert payload["bad_hash_rejected"] is True
    assert payload["backpressure_rejected"] is True
    assert payload["missing_active_layers"] == 0


async def test_stack_manager_under_race_keeps_per_agent_records_consistent(
    native_sandbox: SandboxHandle,
) -> None:
    payload = await run_native_case(
        native_sandbox,
        _RACE_BODY,
        label="layer_stack.stack_manager_integration_under_race",
    )
    assert payload["agents"] == 4
    assert payload["manifest_depth"] == 5
    assert payload["all_leases_released"] is True
