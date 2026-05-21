"""Checkpoint squash behavior for layer-stack manifests."""

from __future__ import annotations

from pathlib import Path

from sandbox.layer_stack import (
    DeleteLayerChange,
    WriteLayerChange,
    LayerStack,
)
from sandbox.layer_stack.manifest import LayerRef, Manifest
from sandbox.layer_stack.squash import CheckpointSegment, SquashService


def _source(tmp_path: Path, name: str, content: bytes) -> str:
    path = tmp_path / "sources" / name
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(content)
    return str(path)


def _publish(
    manager: LayerStack,
    tmp_path: Path,
    rel: str,
    content: bytes,
) -> None:
    manager.publish_changes(
        [
            WriteLayerChange(
                path=rel,
                source_path=_source(tmp_path, rel.replace("/", "_"), content),
            )
        ]
    )


def _layer_path(manager: LayerStack, layer: LayerRef) -> Path:
    return manager.storage_root / layer.path


def test_squash_replaces_unleased_layers_with_checkpoint(tmp_path: Path) -> None:
    manager = LayerStack(tmp_path / "stack")
    _publish(manager, tmp_path, "a.txt", b"a1")
    _publish(manager, tmp_path, "b.txt", b"b1")
    _publish(manager, tmp_path, "a.txt", b"a2")
    before = manager.read_active_manifest()

    manifest = manager.squash(max_depth=2)

    assert manifest is not None
    assert manifest.depth == 1
    assert manifest.layers[0].layer_id.startswith("B")
    assert all(not _layer_path(manager, layer).exists() for layer in before.layers)
    assert manager.read_text("a.txt") == ("a2", True)
    assert manager.read_text("b.txt") == ("b1", True)


def test_squash_plan_collapses_each_unpinned_run_around_pinned_layers(
    tmp_path: Path,
) -> None:
    layers = tuple(
        LayerRef(layer_id=f"L{index:06d}", path=f"layers/L{index:06d}")
        for index in range(7)
    )
    plan = SquashService(tmp_path / "stack").plan(
        Manifest(version=1, layers=layers),
        max_depth=2,
        pinned_layers=(layers[3],),
    )

    assert plan is not None
    assert plan.entries == (
        CheckpointSegment(layers[:3]),
        layers[3],
        CheckpointSegment(layers[4:]),
    )
    assert plan.squashed_layers == (*layers[:3], *layers[4:])
    assert plan.resulting_depth == 3


def test_squash_plan_skips_tiny_runs_when_pinned_suffix_exceeds_threshold(
    tmp_path: Path,
) -> None:
    layers = tuple(
        LayerRef(layer_id=f"L{index:06d}", path=f"layers/L{index:06d}")
        for index in range(8)
    )

    assert (
        SquashService(tmp_path / "stack").plan(
            Manifest(version=1, layers=layers[:6]),
            max_depth=3,
            pinned_layers=layers[2:6],
            min_reduction=2,
        )
        is None
    )

    plan = SquashService(tmp_path / "stack").plan(
        Manifest(version=2, layers=layers),
        max_depth=3,
        pinned_layers=layers[4:],
    )

    assert plan is not None
    assert plan.entries == (CheckpointSegment(layers[:4]), *layers[4:])


def test_squash_checkpoint_preserves_delete_semantics(tmp_path: Path) -> None:
    manager = LayerStack(tmp_path / "stack")
    _publish(manager, tmp_path, "deleted.txt", b"old")
    manager.publish_changes([DeleteLayerChange(path="deleted.txt")])

    manifest = manager.squash(max_depth=1)

    assert manifest is not None
    assert manifest.depth == 1
    assert manager.read_text("deleted.txt") == ("", False)


def test_leased_snapshot_remains_readable_until_release_after_squash(
    tmp_path: Path,
) -> None:
    manager = LayerStack(tmp_path / "stack")
    _publish(manager, tmp_path, "a.txt", b"a1")
    _publish(manager, tmp_path, "b.txt", b"b1")
    _publish(manager, tmp_path, "a.txt", b"a2")
    lease = manager.acquire_snapshot_lease("request-a")
    leased_layers = lease.manifest.layers

    _publish(manager, tmp_path, "c.txt", b"c1")
    _publish(manager, tmp_path, "d.txt", b"d1")
    _publish(manager, tmp_path, "e.txt", b"e1")
    squashed = manager.squash(max_depth=2)

    assert squashed is not None
    assert squashed.depth == 3
    assert squashed.layers[0].layer_id.startswith("B")
    assert squashed.layers[1] == leased_layers[0]
    assert squashed.layers[2].layer_id.startswith("B")
    assert manager.read_text("a.txt", manifest=lease.manifest) == ("a2", True)
    assert manager.read_text("b.txt", manifest=lease.manifest) == ("b1", True)
    assert all(_layer_path(manager, layer).is_dir() for layer in leased_layers)

    assert manager.release_lease(lease.lease_id) is True

    assert _layer_path(manager, leased_layers[0]).is_dir()
    assert all(not _layer_path(manager, layer).exists() for layer in leased_layers[1:])
    final_squash = manager.squash(max_depth=2)

    assert final_squash is not None
    assert final_squash.depth == 1
    assert all(not _layer_path(manager, layer).exists() for layer in leased_layers)
    assert manager.read_text("a.txt") == ("a2", True)
    assert manager.read_text("b.txt") == ("b1", True)
    assert manager.read_text("c.txt") == ("c1", True)
    assert manager.read_text("d.txt") == ("d1", True)
    assert manager.read_text("e.txt") == ("e1", True)
