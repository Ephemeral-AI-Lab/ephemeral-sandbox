"""Tests for the layer-stack workspace base."""

from __future__ import annotations

import os
from pathlib import Path

import pytest

import sandbox.layer_stack.workspace_base as workspace_base_module
from sandbox.layer_stack import LayerStack
from sandbox.layer_stack.workspace_base import (
    WORKSPACE_BASE_LAYER_ID,
    WorkspaceBaseAlreadyExistsError,
    WorkspaceBaseIncompleteError,
    build_workspace_base,
)
from sandbox.layer_stack.workspace_binding import read_workspace_binding


def test_workspace_base_writes_full_manifest_binding(tmp_path: Path) -> None:
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    (workspace / "src").mkdir()
    (workspace / "empty").mkdir()
    (workspace / "src" / "a.py").write_text("print('base')\n", encoding="utf-8")
    (workspace / "README.md").write_text("# demo\n", encoding="utf-8")
    (workspace / "large.bin").write_bytes(b"x" * 9)
    os.symlink("src/a.py", workspace / "link.py")

    stack = tmp_path / "runtime" / "layer-stack"
    binding = build_workspace_base(
        workspace_root=workspace,
        layer_stack_root=stack,
    )

    manager = LayerStack(stack)
    manifest = manager.read_active_manifest()
    assert manifest.version == 1
    assert manifest.layers[0].layer_id == WORKSPACE_BASE_LAYER_ID
    assert binding.active_manifest_version == 1
    assert binding.base_manifest_version == 1
    assert len(binding.active_root_hash) == 64
    assert binding.active_root_hash == binding.base_root_hash

    content, exists = manager.read_text("README.md")
    assert exists is True
    assert content == "# demo\n"
    target, symlink_kind = manager.read_symlink("link.py")
    assert symlink_kind == "symlink"
    assert target == "src/a.py"

    loaded = read_workspace_binding(stack)
    assert loaded == binding
    assert set(binding.to_dict()) == {
        "workspace_root",
        "layer_stack_root",
        "active_manifest_version",
        "active_root_hash",
        "base_manifest_version",
        "base_root_hash",
    }
    assert manager.read_text("src/a.py") == ("print('base')\n", True)
    content, exists = manager.read_bytes("large.bin")
    assert exists is True
    assert content == b"x" * 9
    projected = tmp_path / "projected"
    manager.project(projected)
    assert (projected / "empty").is_dir()


def test_repeated_base_build_fails_unless_reset_requested(tmp_path: Path) -> None:
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    (workspace / "a.txt").write_text("first\n", encoding="utf-8")
    stack = tmp_path / "stack"

    first = build_workspace_base(workspace_root=workspace, layer_stack_root=stack)
    assert first.base_manifest_version == 1

    with pytest.raises(WorkspaceBaseAlreadyExistsError):
        build_workspace_base(workspace_root=workspace, layer_stack_root=stack)

    (workspace / "a.txt").write_text("second\n", encoding="utf-8")
    reset = build_workspace_base(
        workspace_root=workspace,
        layer_stack_root=stack,
        reset=True,
    )
    manager = LayerStack(stack)
    content, exists = manager.read_text("a.txt")
    assert exists is True
    assert content == "second\n"
    assert reset.base_root_hash != first.base_root_hash


def test_workspace_base_fails_when_special_files_prevent_full_copy(
    tmp_path: Path,
) -> None:
    if not hasattr(os, "mkfifo"):
        pytest.skip("mkfifo is unavailable on this platform")
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    os.mkfifo(workspace / "pipe")
    stack = tmp_path / "stack"

    with pytest.raises(WorkspaceBaseIncompleteError) as exc:
        build_workspace_base(workspace_root=workspace, layer_stack_root=stack)

    assert exc.value.special_file_rejections == ("pipe",)
    assert read_workspace_binding(stack) is None


def test_workspace_base_preserves_relative_and_absolute_symlinks(tmp_path: Path) -> None:
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    (workspace / "src").mkdir()
    (workspace / "src" / "a.py").write_text("print('base')\n", encoding="utf-8")
    outside = tmp_path / "outside.txt"
    outside.write_text("outside\n", encoding="utf-8")
    (workspace / "links").mkdir()
    os.symlink("../src/a.py", workspace / "links" / "inside")
    os.symlink(outside, workspace / "links" / "outside")

    stack = tmp_path / "stack"
    build_workspace_base(workspace_root=workspace, layer_stack_root=stack)

    manager = LayerStack(stack)
    assert manager.read_symlink("links/inside") == ("../src/a.py", "symlink")
    assert manager.read_symlink("links/outside") == (outside.as_posix(), "symlink")


def test_workspace_base_has_no_source_control_classification_branches() -> None:
    source = "\n".join(
        [
            Path(workspace_base_module.__file__).read_text(encoding="utf-8"),
        ]
    )

    for needle in (".gitignore", "check-ignore", "track" + "ed", "un" + "track" + "ed"):
        assert needle not in source
