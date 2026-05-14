"""Tests for Phase 03 overlay-capture to OCC changeset conversion."""

from __future__ import annotations

import os

from sandbox.occ.changeset.types import (
    DeleteChange,
    OpaqueDirChange,
    SymlinkChange,
    WriteChange,
)
from sandbox.overlay import OverlayPathChange, content_hash
from sandbox.occ.capture.overlay import overlay_path_changes_to_occ_changes


def test_overlay_path_changes_to_occ_changes_converts_changes(tmp_path) -> None:
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
        ]
    )

    assert isinstance(changes[0], WriteChange)
    assert changes[0].source == "overlay_capture"
    assert changes[0].final_content == b"new"
    assert isinstance(changes[1], DeleteChange)
    assert changes[1].base_hash is None
    assert isinstance(changes[2], SymlinkChange)
    assert changes[2].target == "/target"
    assert isinstance(changes[3], OpaqueDirChange)
    assert changes[3].kept_children == frozenset({"keep.py"})
