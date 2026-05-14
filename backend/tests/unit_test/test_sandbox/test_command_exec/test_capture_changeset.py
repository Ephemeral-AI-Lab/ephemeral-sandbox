"""Command-exec capture to OCC conversion tests."""

from __future__ import annotations

import os
from pathlib import Path

import pytest

from sandbox.occ.capture.overlay import overlay_path_changes_to_occ_changes
from sandbox.occ.changeset.types import (
    DeleteChange,
    OpaqueDirChange,
    SymlinkChange,
    WriteChange,
)
from sandbox.overlay import OverlayPathChange, content_hash


def test_overlay_path_changes_to_occ_changes_converts_all_supported_kinds(
    tmp_path: Path,
) -> None:
    write_path = tmp_path / "new.txt"
    write_path.write_bytes(b"new")
    link_path = tmp_path / "link"
    os.symlink("/target", link_path)

    changes = overlay_path_changes_to_occ_changes(
        [
            OverlayPathChange(
                path="src/new.txt",
                kind="write",
                content_path=str(write_path),
                final_hash=content_hash(write_path),
            ),
            OverlayPathChange(
                path="src/old.txt",
                kind="delete",
                content_path=None,
                final_hash=None,
            ),
            OverlayPathChange(
                path="link",
                kind="symlink",
                content_path=str(link_path),
                final_hash=content_hash(link_path, symlink=True),
            ),
            OverlayPathChange(
                path="dir",
                kind="opaque_dir",
                content_path=None,
                final_hash=None,
            ),
            OverlayPathChange(
                path="dir/keep.py",
                kind="write",
                content_path=str(write_path),
                final_hash=content_hash(write_path),
            ),
            OverlayPathChange(
                path="dir/nested/child.py",
                kind="write",
                content_path=str(write_path),
                final_hash=content_hash(write_path),
            ),
        ]
    )

    assert isinstance(changes[0], WriteChange)
    assert changes[0].source == "overlay_capture"
    # Phase 3 improvement #2: WriteChange threads content_path +
    # precomputed_hash so the OCC stager can `shutil.copyfile` without
    # round-tripping bytes through Python.
    assert changes[0].content_path == str(write_path)
    assert changes[0].precomputed_hash == content_hash(write_path)
    # final_content stays accessible via lazy materialisation so any
    # caller that needs the bytes (e.g. EditChange chained after this
    # WriteChange) gets them on demand.
    assert changes[0].final_content == b"new"
    assert isinstance(changes[1], DeleteChange)
    assert changes[1].base_hash is None
    assert isinstance(changes[2], SymlinkChange)
    assert changes[2].target == "/target"
    assert isinstance(changes[3], OpaqueDirChange)
    assert changes[3].kept_children == frozenset({"keep.py", "nested"})


def test_opaque_dir_kept_children_normalizes_paths(tmp_path: Path) -> None:
    write_path = tmp_path / "keep.py"
    write_path.write_bytes(b"keep")
    opaque = object.__new__(OverlayPathChange)
    object.__setattr__(opaque, "path", "dir/")
    object.__setattr__(opaque, "kind", "opaque_dir")
    object.__setattr__(opaque, "content_path", None)
    object.__setattr__(opaque, "final_hash", None)

    changes = overlay_path_changes_to_occ_changes(
        [
            opaque,
            OverlayPathChange(
                path="dir/keep.py",
                kind="write",
                content_path=str(write_path),
                final_hash=content_hash(write_path),
            ),
        ]
    )

    assert isinstance(changes[0], OpaqueDirChange)
    assert changes[0].kept_children == frozenset({"keep.py"})


def test_overlay_path_changes_to_occ_changes_rejects_missing_content_path() -> None:
    invalid_change = object.__new__(OverlayPathChange)
    object.__setattr__(invalid_change, "path", "src/new.txt")
    object.__setattr__(invalid_change, "kind", "write")
    object.__setattr__(invalid_change, "content_path", None)
    object.__setattr__(invalid_change, "final_hash", None)

    with pytest.raises(ValueError, match="lacks content path"):
        overlay_path_changes_to_occ_changes([invalid_change])


def test_overlay_path_changes_to_occ_changes_rejects_missing_final_hash(
    tmp_path: Path,
) -> None:
    """Phase 3 improvement #2 — final_hash is now a hard requirement.

    The stager uses the precomputed hash to skip re-hashing 8 MiB of
    bytes per write. A missing final_hash means the capture pipeline
    didn't finish hashing — that's an upstream bug, not a recoverable
    case.
    """
    write_path = tmp_path / "x.txt"
    write_path.write_bytes(b"x")
    invalid = object.__new__(OverlayPathChange)
    object.__setattr__(invalid, "path", "src/x.txt")
    object.__setattr__(invalid, "kind", "write")
    object.__setattr__(invalid, "content_path", str(write_path))
    object.__setattr__(invalid, "final_hash", None)

    with pytest.raises(ValueError, match="lacks final_hash"):
        overlay_path_changes_to_occ_changes([invalid])
