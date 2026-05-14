"""Merged-view behavior for frozen layer-stack manifests."""

from __future__ import annotations

import shutil
from pathlib import Path

import pytest

from sandbox.layer_stack import (
    DeleteLayerChange,
    LayerChange,
    LayerStackManager,
    LayerStackStorageError,
    OpaqueDirLayerChange,
    SymlinkLayerChange,
    WriteLayerChange,
)


def _source(tmp_path: Path, name: str, content: bytes) -> str:
    path = tmp_path / "sources" / name
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(content)
    return str(path)


def test_read_uses_leased_manifest_not_advanced_active_manifest(tmp_path: Path) -> None:
    manager = LayerStackManager(tmp_path / "stack")
    manager.publish_changes(
        [
            WriteLayerChange(
                path="pkg/value.txt",
                source_path=_source(tmp_path, "base.txt", b"base"),
            )
        ]
    )
    lease = manager.acquire_snapshot_lease("request-a")

    manager.publish_changes(
        [
            WriteLayerChange(
                path="pkg/value.txt",
                source_path=_source(tmp_path, "new.txt", b"new"),
            )
        ]
    )

    assert manager.read_text("pkg/value.txt") == ("new", True)
    assert manager.read_text("pkg/value.txt", manifest=lease.manifest) == ("base", True)


def test_stale_layer_read_raises_typed_storage_error(tmp_path: Path) -> None:
    manager = LayerStackManager(tmp_path / "stack")
    manifest = manager.publish_changes(
        [
            WriteLayerChange(
                path="pkg/value.txt",
                source_path=_source(tmp_path, "value.txt", b"value"),
            )
        ]
    )
    assert manager.read_bytes("pkg/value.txt", manifest=manifest) == (b"value", True)

    shutil.rmtree(manager.storage_root / manifest.layers[0].path)

    with pytest.raises(LayerStackStorageError) as exc_info:
        manager.read_bytes("pkg/value.txt", manifest=manifest)

    assert exc_info.value.layer_id == manifest.layers[0].layer_id


def test_whiteout_hides_older_file(tmp_path: Path) -> None:
    manager = LayerStackManager(tmp_path / "stack")
    manager.publish_changes(
        [
            WriteLayerChange(
                path="old.txt",
                source_path=_source(tmp_path, "old.txt", b"old"),
            )
        ]
    )
    manager.publish_changes([DeleteLayerChange(path="old.txt")])

    assert manager.read_bytes("old.txt") == (None, False)
    assert manager.list_dir("") == ()


def test_opaque_dir_hides_older_children(tmp_path: Path) -> None:
    manager = LayerStackManager(tmp_path / "stack")
    manager.publish_changes(
        [
            WriteLayerChange(
                path="pkg/a.py",
                source_path=_source(tmp_path, "a.py", b"a"),
            ),
            WriteLayerChange(
                path="pkg/b.py",
                source_path=_source(tmp_path, "b.py", b"b"),
            ),
        ]
    )
    manager.publish_changes(
        [
            OpaqueDirLayerChange(path="pkg"),
            WriteLayerChange(
                path="pkg/new.py",
                source_path=_source(tmp_path, "new.py", b"new"),
            ),
        ]
    )

    assert manager.read_bytes("pkg/a.py") == (None, False)
    assert manager.list_dir("pkg") == ("new.py",)


def test_index_driven_list_dir_handles_files_whiteouts_and_opaque_marker(
    tmp_path: Path,
) -> None:
    """Phase 3 improvement #3 — index-driven list_dir / read_symlink parity.

    Constructs a directory carrying all three of:
      - a live file (``mix/keep.txt``),
      - a whiteout marker (delete of ``mix/gone.txt``, recorded in a
        younger layer as ``mix/.wh.gone.txt``),
      - an opaque-dir marker on a sibling subdirectory.

    The merged view must list only the live file under ``mix``, must
    return ``(None, False)`` for the whited-out path, and must report
    the symlink target unchanged.
    """
    manager = LayerStackManager(tmp_path / "stack")
    # Layer 1: seed gone.txt + keep.txt + a subdir nested/x.txt + a symlink.
    manager.publish_changes(
        [
            WriteLayerChange(
                path="mix/gone.txt",
                source_path=_source(tmp_path, "gone.txt", b"to-be-deleted"),
            ),
            WriteLayerChange(
                path="mix/keep.txt",
                source_path=_source(tmp_path, "keep.txt", b"survives"),
            ),
            WriteLayerChange(
                path="mix/nested/x.txt",
                source_path=_source(tmp_path, "x.txt", b"x"),
            ),
            SymlinkLayerChange(
                path="mix/link",
                source_path="keep.txt",
            ),
        ]
    )
    # Layer 2: delete gone.txt (whiteout) + opaque-marker the nested dir.
    manager.publish_changes(
        [
            DeleteLayerChange(path="mix/gone.txt"),
            OpaqueDirLayerChange(path="mix/nested"),
        ]
    )

    # list_dir under mix must show: keep.txt + link + nested. gone.txt is
    # masked by the whiteout in the youngest layer.
    assert manager.list_dir("mix") == ("keep.txt", "link", "nested")
    # nested is opaque in the youngest layer with no fresh children, so
    # listing it is empty (older x.txt is hidden by the opaque marker).
    assert manager.list_dir("mix/nested") == ()
    # read_bytes on the whited-out path returns (None, False).
    assert manager.read_bytes("mix/gone.txt") == (None, False)
    # read_symlink follows the index path to a symlink target.
    assert manager.read_symlink("mix/link") == ("keep.txt", True)
    # read_symlink on a regular file path returns ("", False) (path
    # exists but is not a symlink).
    assert manager.read_symlink("mix/keep.txt") == ("", False)
    # read_symlink on a path under an opaque-dir ancestor short-circuits
    # to ("", False) without touching the filesystem.
    assert manager.read_symlink("mix/nested/x.txt") == ("", False)


def test_materialize_matches_point_reads_and_preserves_symlinks(tmp_path: Path) -> None:
    manager = LayerStackManager(tmp_path / "stack")
    manager.publish_changes(
        [
            WriteLayerChange(
                path="target.txt",
                source_path=_source(tmp_path, "target.txt", b"target"),
            ),
            SymlinkLayerChange(path="links/current", source_path="../target.txt"),
        ]
    )
    destination = tmp_path / "materialized"

    manager.materialize(destination)

    assert (destination / "target.txt").read_text(encoding="utf-8") == "target"
    assert (destination / "links" / "current").is_symlink()
    assert (destination / "links" / "current").readlink().as_posix() == "../target.txt"
    assert manager.read_symlink("links/current") == ("../target.txt", True)
