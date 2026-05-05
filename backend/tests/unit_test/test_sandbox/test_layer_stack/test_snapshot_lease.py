"""Snapshot lease refcount tests for layer stacks."""

from __future__ import annotations

from pathlib import Path

from sandbox.layer_stack import LayerChange, LayerStackManager


def _source(tmp_path: Path, name: str, content: bytes) -> str:
    path = tmp_path / "sources" / name
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(content)
    return str(path)


def test_acquire_and_release_pin_exact_layer_refs(tmp_path: Path) -> None:
    manager = LayerStackManager(tmp_path / "stack")
    manifest = manager.publish_changes(
        [
            LayerChange(
                path="a.txt",
                kind="write",
                source_path=_source(tmp_path, "a.txt", b"a"),
            )
        ]
    )
    top_layer = manifest.layers[0]

    lease_a = manager.acquire_snapshot_lease("request-a")
    lease_b = manager.acquire_snapshot_lease("request-b")

    assert lease_a.manifest == manifest
    assert lease_b.manifest == manifest
    assert manager.lease_refcount(top_layer) == 2
    assert manager.pinned_layers() == (top_layer,)

    assert manager.release_lease(lease_a.lease_id) is True
    assert manager.lease_refcount(top_layer) == 1
    assert manager.release_lease(lease_a.lease_id) is False
    assert manager.lease_refcount(top_layer) == 1

    assert manager.release_lease(lease_b.lease_id) is True
    assert manager.lease_refcount(top_layer) == 0
    assert manager.pinned_layers() == ()


def test_releasing_old_snapshot_does_not_unpin_new_active_layer(tmp_path: Path) -> None:
    manager = LayerStackManager(tmp_path / "stack")
    old_manifest = manager.publish_changes(
        [
            LayerChange(
                path="a.txt",
                kind="write",
                source_path=_source(tmp_path, "old.txt", b"old"),
            )
        ]
    )
    old_lease = manager.acquire_snapshot_lease("old-request")
    new_manifest = manager.publish_changes(
        [
            LayerChange(
                path="b.txt",
                kind="write",
                source_path=_source(tmp_path, "new.txt", b"new"),
            )
        ]
    )
    new_lease = manager.acquire_snapshot_lease("new-request")

    assert manager.lease_refcount(old_manifest.layers[0]) == 2
    assert manager.lease_refcount(new_manifest.layers[0]) == 1

    manager.release_lease(old_lease.lease_id)

    assert manager.lease_refcount(old_manifest.layers[0]) == 1
    assert manager.lease_refcount(new_manifest.layers[0]) == 1
    assert manager.release_lease(new_lease.lease_id) is True


def test_expiring_leases_releases_refcounts_in_age_order(tmp_path: Path) -> None:
    manager = LayerStackManager(tmp_path / "stack")
    manifest = manager.publish_changes(
        [
            LayerChange(
                path="a.txt",
                kind="write",
                source_path=_source(tmp_path, "a.txt", b"a"),
            )
        ]
    )

    first = manager.acquire_snapshot_lease("request-a")
    second = manager.acquire_snapshot_lease("request-b")
    expired = manager.expire_leases_older_than(
        1.0,
        now=second.acquired_at + 2.0,
    )

    assert expired == (first, second)
    assert manager.lease_refcount(manifest.layers[0]) == 0
    assert manager.pinned_layers() == ()


def test_sweeping_dead_lease_owners_keeps_live_refcounts(tmp_path: Path) -> None:
    manager = LayerStackManager(tmp_path / "stack")
    manifest = manager.publish_changes(
        [
            LayerChange(
                path="a.txt",
                kind="write",
                source_path=_source(tmp_path, "a.txt", b"a"),
            )
        ]
    )

    live = manager.acquire_snapshot_lease("request-live")
    dead = manager.acquire_snapshot_lease("request-dead")
    swept = manager.sweep_dead_lease_owners(("request-live",))

    assert swept == (dead,)
    assert manager.lease_refcount(manifest.layers[0]) == 1
    assert manager.pinned_layers() == manifest.layers
    assert manager.release_lease(live.lease_id) is True
