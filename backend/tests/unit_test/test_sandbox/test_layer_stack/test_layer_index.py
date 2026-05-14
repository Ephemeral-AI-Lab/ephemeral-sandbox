"""Per-layer presence index unit tests."""

from __future__ import annotations

from pathlib import Path

from sandbox.layer_stack import (
    DeleteLayerChange,
    WriteLayerChange,
    LayerStackManager,
)
from sandbox.layer_stack.layer_index import (
    OPAQUE_MARKER,
    WHITEOUT_PREFIX,
    build_layer_index,
    has_ancestor_in,
)


def _source(tmp_path: Path, name: str, content: bytes) -> str:
    path = tmp_path / "sources" / name
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(content)
    return str(path)


def test_build_layer_index_classifies_files_whiteouts_and_opaque_dirs(
    tmp_path: Path,
) -> None:
    layer_dir = tmp_path / "layer"
    layer_dir.mkdir()
    (layer_dir / "src").mkdir()
    (layer_dir / "src" / "app.py").write_text("hello")
    (layer_dir / "dist").mkdir()
    (layer_dir / "dist" / f"{WHITEOUT_PREFIX}foo.txt").write_text("")
    (layer_dir / "pkg").mkdir()
    (layer_dir / "pkg" / OPAQUE_MARKER).write_text("")

    index = build_layer_index(layer_dir)

    assert "src/app.py" in index.files
    assert "dist/foo.txt" in index.whiteouts
    assert "pkg" in index.opaque_dirs


def test_opaque_marker_does_not_register_as_whiteout(tmp_path: Path) -> None:
    layer_dir = tmp_path / "layer"
    layer_dir.mkdir()
    (layer_dir / OPAQUE_MARKER).write_text("")

    index = build_layer_index(layer_dir)

    assert "" in index.opaque_dirs
    assert all(name != ".opq" for name in index.whiteouts)


def test_has_ancestor_in_walks_strict_ancestors() -> None:
    members = frozenset({"a", "a/b"})
    assert has_ancestor_in("a/b/c", members) is True
    assert has_ancestor_in("a/x", members) is True
    assert has_ancestor_in("a", members) is False  # strict ancestors only
    assert has_ancestor_in("z", members) is False


def test_indexed_read_handles_nested_whiteout(tmp_path: Path) -> None:
    """`dist/.wh.foo` must hide `dist/foo` from older layers via the index."""
    manager = LayerStackManager(tmp_path / "stack")
    manager.publish_changes(
        [
            WriteLayerChange(
                path="dist/foo",
                source_path=_source(tmp_path, "foo.txt", b"original"),
            )
        ]
    )
    manager.publish_changes([DeleteLayerChange(path="dist/foo")])

    assert manager.read_bytes("dist/foo") == (None, False)


def test_indexed_read_evicts_after_layer_removal(tmp_path: Path) -> None:
    """Cache eviction wired through `_remove_unreferenced_layers`."""
    manager = LayerStackManager(tmp_path / "stack")
    manager.publish_changes(
        [
            WriteLayerChange(
                path="x.txt",
                source_path=_source(tmp_path, "x.txt", b"v1"),
            )
        ]
    )
    # Squash collapses earlier layers into a checkpoint, dropping the original
    # layer_id; the cache must drop it too so memory does not leak.
    pre_squash_layers = tuple(
        layer.layer_id for layer in manager.read_active_manifest().layers
    )
    manager.publish_changes(
        [
            WriteLayerChange(
                path="x.txt",
                source_path=_source(tmp_path, "x2.txt", b"v2"),
            )
        ]
    )
    # Acquire+release a lease to trigger the unreferenced-layer GC path.
    lease = manager.acquire_snapshot_lease("test-evict")
    manager.release_lease(lease.lease_id)

    cache = manager._view._layer_index_cache
    for layer_id in pre_squash_layers:
        # Either still active in current manifest or evicted — never both.
        active = any(
            layer.layer_id == layer_id
            for layer in manager.read_active_manifest().layers
        )
        if not active:
            assert layer_id not in cache, (
                f"cache leaked dropped layer {layer_id}: {sorted(cache)}"
            )
