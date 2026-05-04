"""Checkpoint squash behavior for layer-stack manifests."""

from __future__ import annotations

from pathlib import Path

from sandbox.layer_stack import LayerChange, LayerStackManager
from sandbox.layer_stack.manifest import LayerRef


def _source(tmp_path: Path, name: str, content: bytes) -> str:
    path = tmp_path / "sources" / name
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(content)
    return str(path)


def _publish(
    manager: LayerStackManager,
    tmp_path: Path,
    rel: str,
    content: bytes,
) -> None:
    manager.publish_changes(
        [
            LayerChange(
                path=rel,
                kind="write",
                source_path=_source(tmp_path, rel.replace("/", "_"), content),
            )
        ]
    )


def _layer_path(manager: LayerStackManager, layer: LayerRef) -> Path:
    return manager.storage_root / layer.path


def test_squash_replaces_old_active_suffix_with_checkpoint(tmp_path: Path) -> None:
    manager = LayerStackManager(tmp_path / "stack")
    _publish(manager, tmp_path, "a.txt", b"a1")
    _publish(manager, tmp_path, "b.txt", b"b1")
    _publish(manager, tmp_path, "a.txt", b"a2")

    manifest = manager.squash(max_depth=2)

    assert manifest is not None
    assert manifest.depth == 2
    assert manifest.layers[-1].layer_id.startswith("B")
    assert manager.read_text("a.txt") == ("a2", True)
    assert manager.read_text("b.txt") == ("b1", True)


def test_squash_checkpoint_preserves_delete_semantics(tmp_path: Path) -> None:
    manager = LayerStackManager(tmp_path / "stack")
    _publish(manager, tmp_path, "deleted.txt", b"old")
    manager.publish_changes([LayerChange(path="deleted.txt", kind="delete")])

    manifest = manager.squash(max_depth=1)

    assert manifest is not None
    assert manifest.depth == 1
    assert manager.read_text("deleted.txt") == ("", False)


def test_leased_snapshot_remains_readable_after_squash_and_gc(tmp_path: Path) -> None:
    manager = LayerStackManager(tmp_path / "stack")
    _publish(manager, tmp_path, "a.txt", b"a1")
    _publish(manager, tmp_path, "b.txt", b"b1")
    _publish(manager, tmp_path, "a.txt", b"a2")
    lease = manager.acquire_snapshot_lease("request-a")
    leased_layers = lease.manifest.layers

    _publish(manager, tmp_path, "c.txt", b"c1")
    squashed = manager.squash(max_depth=2)

    assert squashed is not None
    assert squashed.depth == 2
    assert manager.read_text("a.txt", manifest=lease.manifest) == ("a2", True)
    assert manager.read_text("b.txt", manifest=lease.manifest) == ("b1", True)
    assert all(_layer_path(manager, layer).is_dir() for layer in leased_layers)

    assert manager.release_lease(lease.lease_id) is True
    manager.collect_garbage(young_staging_age_seconds=0)

    assert all(not _layer_path(manager, layer).exists() for layer in leased_layers)
    assert manager.read_text("a.txt") == ("a2", True)
    assert manager.read_text("b.txt") == ("b1", True)
    assert manager.read_text("c.txt") == ("c1", True)
