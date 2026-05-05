"""E6 — layer GC and lease interaction.

Backs §4.1. Pass bar: 0 false-frees over 100 runs; retired-but-pinned
layers reclaimed exactly once on lease release.
"""

from __future__ import annotations

from pathlib import Path

from .._harness.assertions import assert_manifest_layers_referenced_on_disk
from .._harness.sandbox_fixture import SandboxHandle
from .._harness.workload import (
    acquire_lease,
    commit_layer,
    commit_layers,
    layer_paths,
    release_lease,
    squash_to,
)


def _layer_dir_names(manager) -> set[str]:
    return {
        child.name
        for child in (manager.storage_root / "layers").iterdir()
        if child.is_dir()
    }


def test_leased_layer_not_gced_until_release(
    layer_stack_sandbox: SandboxHandle, tmp_path: Path
) -> None:
    """A snapshot lease held against a now-retired layer must keep the
    layer dir on disk; gc must reclaim it on the very next sweep after
    the lease releases."""
    manager = layer_stack_sandbox.layer_stack
    assert manager is not None
    payloads = tmp_path / "payloads"

    commit_layers(manager, payloads, 6)
    pre_squash = manager.read_active_manifest()
    pinned = set(layer_paths(pre_squash))

    lease = acquire_lease(manager, owner_id="reader-a")

    new_manifest = squash_to(manager, max_depth=2)
    assert new_manifest is not None
    assert new_manifest.depth <= 2

    # While the lease is held, none of the originally-pinned layer dirs
    # may be reclaimed even though most are no longer in the active manifest.
    on_disk = _layer_dir_names(manager)
    pinned_names = {Path(path).name for path in pinned}
    assert pinned_names <= on_disk, (
        f"gc removed leased layers: missing={pinned_names - on_disk}"
    )

    released = release_lease(manager, lease)
    assert released is True

    fsck = manager.collect_garbage()
    survivors = _layer_dir_names(manager)
    active_names = {
        Path(layer.path).name for layer in manager.read_active_manifest().layers
    }
    # Every retired-but-formerly-pinned layer must be gone after the
    # post-release sweep.
    assert (pinned_names - active_names).isdisjoint(survivors)
    # And the sweep must have actually attributed the freed dirs to fsck.
    assert set(fsck.orphan_layers_removed) >= (pinned_names - active_names)
    assert_manifest_layers_referenced_on_disk(manager)


def test_unreferenced_squashed_layer_freed_within_one_sweep(
    layer_stack_sandbox: SandboxHandle, tmp_path: Path
) -> None:
    """With no leases held, a single gc sweep after squash must remove
    every layer dir not in the new active manifest."""
    manager = layer_stack_sandbox.layer_stack
    assert manager is not None
    payloads = tmp_path / "payloads"

    commit_layers(manager, payloads, 8)
    before_squash = _layer_dir_names(manager)

    new_manifest = squash_to(manager, max_depth=3, collect_garbage=False)
    assert new_manifest is not None
    active_names = {Path(layer.path).name for layer in new_manifest.layers}
    expected_orphans = before_squash - active_names
    assert expected_orphans, "squash should have produced retired layer dirs"

    fsck = manager.collect_garbage()

    after = _layer_dir_names(manager)
    assert after == active_names, (
        f"gc did not converge in one sweep: leftover={after - active_names}"
    )
    assert set(fsck.orphan_layers_removed) == expected_orphans
    assert_manifest_layers_referenced_on_disk(manager)


def test_pinned_layer_survives_squash_until_lease_drops(
    layer_stack_sandbox: SandboxHandle, tmp_path: Path
) -> None:
    """A layer ref pinned by an active lease must survive a squash plus a
    follow-up gc sweep, then be freed exactly once on lease release."""
    manager = layer_stack_sandbox.layer_stack
    assert manager is not None
    payloads = tmp_path / "payloads"

    commit_layer(manager, payloads, "anchor", body="anchor-body\n")
    anchor_manifest = manager.read_active_manifest()
    anchor_names = {Path(layer.path).name for layer in anchor_manifest.layers}

    lease = acquire_lease(manager, owner_id="long-shell")

    commit_layers(manager, payloads, 6, prefix="post")
    squashed = squash_to(manager, max_depth=2)
    assert squashed is not None
    active_names = {Path(layer.path).name for layer in squashed.layers}

    # The lease pins the manifest from before the squash; its layer dirs
    # must remain on disk even though they're not in the active manifest.
    on_disk_pre_release = _layer_dir_names(manager)
    assert anchor_names <= on_disk_pre_release
    assert anchor_names.isdisjoint(active_names), (
        "test setup expected the anchor layer to retire after squash"
    )

    # Repeated sweeps must be idempotent while the lease is held.
    manager.collect_garbage()
    manager.collect_garbage()
    assert anchor_names <= _layer_dir_names(manager)

    assert release_lease(manager, lease) is True
    fsck = manager.collect_garbage()
    assert anchor_names.issubset(set(fsck.orphan_layers_removed))
    assert anchor_names.isdisjoint(_layer_dir_names(manager))
    assert_manifest_layers_referenced_on_disk(manager)
