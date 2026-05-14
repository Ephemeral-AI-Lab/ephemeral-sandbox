"""Lease-pinning invariant — Phase 3 verification D.

`LayerStackManager._remove_unreferenced_layers` is the sole site that
calls `MergedView.evict_layer_index`. The system-design invariant from
Phase 2.5 §"System-design invariants to preserve" is:

> An evicted layer-id MUST NOT still be pinned by any active lease.

Violating this lets a concurrent reader fall back to a layer index built
from a layer dir that has already been removed — silent corruption.

The test builds a 5-layer manifest, holds 4 leases on different
historical manifest versions, churns the active manifest by appending
and removing leaf layers, and asserts that `evict_layer_index` is
never called for a layer-id pinned by any of the live leases. Once
all leases release, the eviction set must equal exactly the layers no
live manifest still references.
"""

from __future__ import annotations

from pathlib import Path

import pytest

from sandbox.layer_stack import (
    DeleteLayerChange,
    WriteLayerChange,
    LayerStackManager,
)


def _source(tmp_path: Path, name: str, content: bytes) -> str:
    path = tmp_path / "sources" / name
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(content)
    return str(path)


def _publish_layer(
    manager: LayerStackManager,
    *,
    tmp_path: Path,
    label: str,
    content: bytes,
) -> str:
    """Publish a single-file layer and return its layer_id."""
    manager.publish_changes(
        [
            WriteLayerChange(
                path=f"pkg/{label}.txt",
                source_path=_source(tmp_path, f"{label}.txt", content),
            )
        ]
    )
    active = manager.read_active_manifest()
    return active.layers[0].layer_id


@pytest.fixture
def evict_log(monkeypatch: pytest.MonkeyPatch) -> list[str]:
    """Capture every `MergedView.evict_layer_index` call site-wide."""
    log: list[str] = []
    import sandbox.layer_stack.view.merged as merged_view_mod

    original = merged_view_mod.MergedView.evict_layer_index

    def _spy(self: merged_view_mod.MergedView, layer_id: str) -> None:
        log.append(layer_id)
        return original(self, layer_id)

    monkeypatch.setattr(merged_view_mod.MergedView, "evict_layer_index", _spy)
    return log


def test_eviction_skips_layers_pinned_by_active_leases(
    tmp_path: Path,
    evict_log: list[str],
) -> None:
    """4 active leases on historical manifests block eviction of their layers."""
    manager = LayerStackManager(tmp_path / "stack")

    # Build 5 layers L1..L5 (publisher prepends the newest, so layers[0]
    # in the active manifest is L5 — but we keep our own L1..L5 mapping).
    layer_ids: list[str] = []
    for index in range(1, 6):
        manager.publish_changes(
            [
                WriteLayerChange(
                    path=f"pkg/layer{index}.txt",
                    source_path=_source(
                        tmp_path, f"layer{index}.txt", f"v{index}".encode()
                    ),
                )
            ]
        )
        layer_ids.append(manager.read_active_manifest().layers[0].layer_id)

    assert len(set(layer_ids)) == 5
    assert evict_log == [], "no eviction should fire while no leases churn"

    # Acquire 4 leases — each pins all 5 layers in the manifest at the
    # time of acquisition. Leases acquired sequentially see the same
    # active manifest until the next publish_changes call.
    leases = [manager.acquire_snapshot_lease(f"lease-{i}") for i in range(4)]
    pinned_layer_ids = {layer.layer_id for layer in manager.pinned_layers()}
    assert pinned_layer_ids == set(layer_ids)

    # Churn: append a new layer L6, then a delete that removes layer L1's
    # contribution (delete is itself a new layer L7 with a whiteout).
    manager.publish_changes(
        [
            WriteLayerChange(
                path="pkg/layer6.txt",
                source_path=_source(tmp_path, "layer6.txt", b"v6"),
            )
        ]
    )
    manager.publish_changes([DeleteLayerChange(path="pkg/layer1.txt")])

    # Eviction can only fire from `release_lease` or `squash`. So far we
    # have only published; the eviction log must still be empty.
    assert evict_log == [], (
        f"publish_changes must not evict; saw {evict_log}"
    )

    # Release the first lease. The other 3 still pin the original 5 layers,
    # plus they all transitively pin the same 5 set. So eviction must not
    # touch any of layer_ids[0..4] yet.
    manager.release_lease(leases[0].lease_id)
    pinned_after_first_release = {layer.layer_id for layer in manager.pinned_layers()}
    assert pinned_after_first_release == set(layer_ids), (
        "remaining 3 leases must keep all original 5 layers pinned"
    )
    pinned_evicted = pinned_after_first_release & set(evict_log)
    assert pinned_evicted == set(), (
        f"eviction touched a still-pinned layer: {sorted(pinned_evicted)}"
    )

    # Release leases 1 and 2 — still 1 lease holding the original 5 pinned.
    manager.release_lease(leases[1].lease_id)
    manager.release_lease(leases[2].lease_id)
    pinned_now = {layer.layer_id for layer in manager.pinned_layers()}
    assert pinned_now == set(layer_ids), "last lease still pins all 5"
    assert set(evict_log) & pinned_now == set(), (
        f"eviction touched still-pinned layer; log={evict_log} pinned={sorted(pinned_now)}"
    )

    # Release the final lease — only now can layers no live manifest
    # still references be evicted. Layer L1's path got whited out (the
    # active manifest's bottom is the whiteout layer + L2..L6 from the
    # publisher's newest-first stacking, depending on publisher policy).
    manager.release_lease(leases[3].lease_id)

    active_after = {layer.layer_id for layer in manager.read_active_manifest().layers}
    pinned_after_full_release = {
        layer.layer_id for layer in manager.pinned_layers()
    }
    assert pinned_after_full_release == set(), "no leases left → no pinned layers"

    # Eviction set must be exactly the layers that are no longer in the
    # active manifest after all leases released.
    actually_evicted = set(evict_log)
    expected_evicted = set(layer_ids) - active_after
    assert expected_evicted.issubset(actually_evicted), (
        f"layers no longer in active manifest must be evicted; "
        f"missing={sorted(expected_evicted - actually_evicted)}"
    )
    # No layer that is STILL in the active manifest may be in the
    # eviction set — that would mean evict_layer_index ran for a live
    # layer.
    live_evicted = active_after & actually_evicted
    assert live_evicted == set(), (
        f"eviction touched live layer(s) in active manifest: {sorted(live_evicted)}"
    )


def test_eviction_strict_set_after_squash(
    tmp_path: Path,
    evict_log: list[str],
) -> None:
    """Squash-driven eviction must respect leases on pre-squash layer ids."""
    manager = LayerStackManager(tmp_path / "stack")
    layer_ids: list[str] = []
    for index in range(1, 5):
        manager.publish_changes(
            [
                WriteLayerChange(
                    path=f"pkg/v{index}.txt",
                    source_path=_source(tmp_path, f"v{index}.txt", str(index).encode()),
                )
            ]
        )
        layer_ids.append(manager.read_active_manifest().layers[0].layer_id)

    # Hold a lease on the pre-squash manifest so squash CANNOT evict its
    # constituent layers.
    held = manager.acquire_snapshot_lease("hold-pre-squash")

    # Squash to depth=1 collapses everything older than the most recent layer.
    manager.squash(max_depth=1)

    # Squash must not evict any pre-squash layer that is still pinned
    # by the held lease.
    pinned_now = {layer.layer_id for layer in manager.pinned_layers()}
    assert pinned_now == set(layer_ids), (
        "lease on pre-squash manifest must keep all 4 layers pinned"
    )
    assert set(evict_log) & pinned_now == set(), (
        f"squash evicted a pinned layer; log={evict_log} pinned={sorted(pinned_now)}"
    )

    manager.release_lease(held.lease_id)

    # After release, layers no longer in active (the squashed-out tail)
    # must finally be evicted. The active manifest after squash collapses
    # the older 3 layers into a single checkpoint.
    active_after = {
        layer.layer_id for layer in manager.read_active_manifest().layers
    }
    expected_evicted = set(layer_ids) - active_after
    assert expected_evicted.issubset(set(evict_log)), (
        f"missing eviction for layers no longer in active manifest: "
        f"{sorted(expected_evicted - set(evict_log))}; "
        f"log={evict_log} active={sorted(active_after)}"
    )
