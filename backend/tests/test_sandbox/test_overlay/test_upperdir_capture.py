"""Phase 02 upperdir capture tests."""

from __future__ import annotations

import hashlib
import os
from pathlib import Path

from sandbox.layer_stack.manifest import Manifest
from sandbox.layer_stack.merged_view import OPAQUE_MARKER, WHITEOUT_PREFIX
from sandbox.overlay.capture.upperdir import capture_changes


def test_upperdir_capture_emits_raw_runtime_changes(tmp_path: Path) -> None:
    upper = tmp_path / "upper"
    upper.mkdir()
    (upper / "app.py").write_text("new\n", encoding="utf-8")
    (upper / f"{WHITEOUT_PREFIX}old.py").write_text("", encoding="utf-8")
    (upper / "pkg").mkdir()
    (upper / "pkg" / OPAQUE_MARKER).write_text("", encoding="utf-8")
    os.symlink("app.py", upper / "current")

    changes = capture_changes(
        upper,
        snapshot_manifest=Manifest(version=3, layers=()),
    )

    by_path = {change.path: change for change in changes}
    assert by_path["app.py"].kind == "write"
    assert by_path["app.py"].final_hash == hashlib.sha256(b"new\n").hexdigest()
    assert by_path["old.py"].kind == "delete"
    assert by_path["old.py"].content_path is None
    assert by_path["pkg"].kind == "opaque_dir"
    assert by_path["current"].kind == "symlink"
    assert by_path["current"].final_hash == hashlib.sha256(b"app.py").hexdigest()
    assert not hasattr(by_path["app.py"], "base_bytes")
    assert not hasattr(by_path["app.py"], "gitignore")


def test_copy_backed_capture_detects_writes_and_deletes(tmp_path: Path) -> None:
    lower = tmp_path / "lower"
    merged = tmp_path / "merged"
    upper = tmp_path / "upper"
    (lower / "pkg").mkdir(parents=True)
    (merged / "pkg").mkdir(parents=True)
    (lower / "pkg" / "value.txt").write_text("old\n", encoding="utf-8")
    (lower / "pkg" / "gone.txt").write_text("gone\n", encoding="utf-8")
    (merged / "pkg" / "value.txt").write_text("new\n", encoding="utf-8")

    changes = capture_changes(
        upper,
        snapshot_manifest=Manifest(version=1, layers=()),
        lowerdir=lower,
        workspace_root=merged,
    )

    by_path = {change.path: change for change in changes}
    assert by_path["pkg/value.txt"].kind == "write"
    assert Path(str(by_path["pkg/value.txt"].content_path)).read_text() == "new\n"
    assert by_path["pkg/gone.txt"].kind == "delete"
