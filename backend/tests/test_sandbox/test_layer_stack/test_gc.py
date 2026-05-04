"""Lease-aware layer-stack garbage collection tests."""

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


def test_gc_keeps_active_and_exact_leased_layers(tmp_path: Path) -> None:
    manager = LayerStackManager(tmp_path / "stack")
    _publish(manager, tmp_path, "a.txt", b"a")
    _publish(manager, tmp_path, "b.txt", b"b")
    _publish(manager, tmp_path, "c.txt", b"c")
    lease = manager.acquire_snapshot_lease("request-a")
    leased_layers = lease.manifest.layers
    orphan = manager.storage_root / "layers" / "orphan"
    orphan.mkdir()

    manager.squash(max_depth=1, collect_garbage=False)
    result = manager.collect_garbage(young_staging_age_seconds=0)

    assert result.orphan_layers_removed == ("orphan",)
    assert all(_layer_path(manager, layer).is_dir() for layer in leased_layers)
    assert manager.read_text("a.txt", manifest=lease.manifest) == ("a", True)

    manager.release_lease(lease.lease_id)
    result = manager.collect_garbage(young_staging_age_seconds=0)

    assert set(result.orphan_layers_removed) == {layer.layer_id for layer in leased_layers}
    assert all(not _layer_path(manager, layer).exists() for layer in leased_layers)
