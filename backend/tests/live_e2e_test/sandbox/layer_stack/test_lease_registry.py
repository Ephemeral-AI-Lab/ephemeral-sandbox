"""Phase 2 native probes for layer-stack lease registry behavior."""

from __future__ import annotations

import pytest

from .._harness.native_cases import run_native_case
from .._harness.sandbox_fixture import SandboxHandle


pytestmark = pytest.mark.asyncio


_REGISTRY_BODY = r"""
from sandbox.layer_stack.lease_registry import LeaseRegistry
from sandbox.layer_stack.manifest import LayerRef, Manifest

label = "layer_stack.lease_registry"
before = sample_resource()
started = time.perf_counter()
root = _case_root(label)
del root
clock = {"now": 100.0}
ids = iter(["lease-a", "lease-b", "lease-live", "lease-dead"])
registry = LeaseRegistry(id_factory=lambda: next(ids), clock=lambda: clock["now"])
layer = LayerRef(layer_id="L000001", path="layers/L000001")
manifest = Manifest(version=1, layers=(layer,))

lease_a = registry.acquire(manifest, "owner-a")
assert lease_a.lease_id == "lease-a"
assert registry.refcount(layer) == 1
released = registry.release(lease_a.lease_id)
double_release = registry.release(lease_a.lease_id)
assert released == lease_a
assert double_release is None
assert registry.refcount(layer) == 0

clock["now"] = 120.0
lease_b = registry.acquire(manifest, "owner-b")
expired = registry.expire_older_than(5.0, now=126.0)
assert expired == (lease_b,)
assert registry.active_leases() == ()
assert registry.refcount(layer) == 0

clock["now"] = 130.0
live = registry.acquire(manifest, "owner-live")
dead = registry.acquire(manifest, "owner-dead")
swept = registry.sweep_dead_owners(["owner-live"])
assert swept == (dead,)
assert registry.active_leases() == (live,)
assert registry.refcount(layer) == 1

_emit(label, started, before, {
    "released": released.lease_id,
    "double_release_is_none": double_release is None,
    "expired": [lease.lease_id for lease in expired],
    "swept_dead_owners": [lease.owner_id for lease in swept],
    "remaining_leases": [lease.lease_id for lease in registry.active_leases()],
    "final_refcount": registry.refcount(layer),
})
"""


_RACE_BODY = r"""
from sandbox.layer_stack.lease_registry import LeaseRegistry
from sandbox.layer_stack.manifest import LayerRef, Manifest

label = "layer_stack.lease_registry_under_race"
before = sample_resource()
started = time.perf_counter()
root = _case_root(label)
del root
registry = LeaseRegistry()
layer = LayerRef(layer_id="L000001", path="layers/L000001")
manifest = Manifest(version=1, layers=(layer,))
n = 16
barrier = threading.Barrier(n)

def register_one(index):
    barrier.wait(timeout=5)
    lease = registry.acquire(manifest, "owner-%02d" % index)
    return lease.lease_id

with concurrent.futures.ThreadPoolExecutor(max_workers=n) as pool:
    lease_ids = list(pool.map(register_one, range(n)))

assert len(set(lease_ids)) == n, lease_ids
assert registry.refcount(layer) == n
assert len(registry.active_leases()) == n

with concurrent.futures.ThreadPoolExecutor(max_workers=n) as pool:
    released = list(pool.map(registry.release, lease_ids))

assert all(lease is not None for lease in released)
assert registry.refcount(layer) == 0
assert registry.active_leases() == ()

_emit(label, started, before, {
    "registered": n,
    "unique_lease_ids": len(set(lease_ids)),
    "released": sum(1 for lease in released if lease is not None),
    "final_refcount": registry.refcount(layer),
    "active_leases": len(registry.active_leases()),
})
"""


async def test_lease_registry_registers_releases_expires_and_sweeps_dead_owners(
    native_sandbox: SandboxHandle,
) -> None:
    payload = await run_native_case(
        native_sandbox,
        _REGISTRY_BODY,
        label="layer_stack.lease_registry",
    )
    assert payload["double_release_is_none"] is True
    assert payload["expired"] == ["lease-b"]
    assert payload["swept_dead_owners"] == ["owner-dead"]
    assert payload["final_refcount"] == 1


async def test_lease_registry_under_race_allocates_unique_leases(
    native_sandbox: SandboxHandle,
) -> None:
    payload = await run_native_case(
        native_sandbox,
        _RACE_BODY,
        label="layer_stack.lease_registry_under_race",
    )
    assert payload["registered"] == 16
    assert payload["unique_lease_ids"] == 16
    assert payload["final_refcount"] == 0
