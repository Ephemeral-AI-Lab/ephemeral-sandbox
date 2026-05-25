"""Workspace lease registry tests for manifest layer pinning."""

from __future__ import annotations

from sandbox.layer_stack.lease import LeaseRegistry
from sandbox.layer_stack.manifest import LayerRef, Manifest


def test_workspace_leases_refcount_manifest_layers() -> None:
    ids = iter(("lease-a", "lease-b"))
    registry = LeaseRegistry(id_factory=lambda: next(ids))
    manifest = Manifest(
        version=3,
        layers=(LayerRef(layer_id="L000003", path="layers/L000003"),),
    )

    lease_a = registry.acquire(manifest, "request-a")
    lease_b = registry.acquire(manifest, "request-b")

    assert lease_a.manifest.version == 3
    assert registry.pinned_layers() == manifest.layers
    assert registry.active_count() == 2

    assert registry.release(lease_a.lease_id) == lease_a
    assert registry.pinned_layers() == manifest.layers

    assert registry.release(lease_b.lease_id) == lease_b
    assert registry.pinned_layers() == ()
    assert registry.active_count() == 0


def test_releasing_unknown_lease_returns_none() -> None:
    registry = LeaseRegistry(id_factory=lambda: "lease-a")
    assert registry.release("missing") is None


def test_squash_barrier_layers_use_only_newest_layer_per_lease() -> None:
    ids = iter(("lease-a", "lease-b", "lease-c", "lease-d"))
    registry = LeaseRegistry(id_factory=lambda: next(ids))
    layer_1 = LayerRef(layer_id="L000001", path="layers/L000001")
    layer_2 = LayerRef(layer_id="L000002", path="layers/L000002")
    layer_3 = LayerRef(layer_id="L000003", path="layers/L000003")

    registry.acquire(
        Manifest(version=3, layers=(layer_3, layer_2, layer_1)),
        "request-a",
    )
    registry.acquire(
        Manifest(version=2, layers=(layer_2, layer_1)),
        "request-b",
    )
    registry.acquire(
        Manifest(version=3, layers=(layer_3, layer_2, layer_1)),
        "request-c",
    )
    registry.acquire(Manifest(version=0, layers=()), "request-d")

    assert registry.squash_barrier_layers() == (layer_2, layer_3)
    assert registry.pinned_layers() == (layer_1, layer_2, layer_3)
