"""Tests for durable workspace binding behavior."""

from __future__ import annotations

from pathlib import Path

import pytest

from sandbox.layer_stack.workspace_base import build_workspace_base
from sandbox.layer_stack.workspace import (
    WorkspaceBinding,
    WorkspaceBindingError,
    require_workspace_binding,
    validate_workspace_binding_paths,
    write_workspace_binding_atomic,
)
from sandbox.runtime import api_handlers


def test_binding_rejects_layer_stack_inside_workspace(tmp_path: Path) -> None:
    workspace = tmp_path / "workspace"
    stack = workspace / ".runtime" / "layer-stack"

    with pytest.raises(WorkspaceBindingError, match="outside workspace_root"):
        validate_workspace_binding_paths(
            workspace_root=workspace,
            layer_stack_root=stack,
        )


def test_binding_round_trips_and_translates_workspace_paths(tmp_path: Path) -> None:
    workspace = tmp_path / "workspace"
    stack = tmp_path / "runtime" / "layer-stack"
    workspace.mkdir()
    binding = WorkspaceBinding(
        workspace_root=workspace.as_posix(),
        layer_stack_root=stack.as_posix(),
        active_manifest_version=1,
        active_root_hash="a" * 64,
        base_manifest_version=1,
        base_root_hash="a" * 64,
    )

    write_workspace_binding_atomic(stack, binding)

    loaded = require_workspace_binding(stack)
    assert loaded == binding
    assert loaded.relative_layer_path("pkg/a.py") == "pkg/a.py"
    assert loaded.relative_layer_path((workspace / "pkg" / "a.py").as_posix()) == "pkg/a.py"
    with pytest.raises(WorkspaceBindingError, match="outside bound workspace"):
        loaded.relative_layer_path("/other/pkg/a.py")


@pytest.mark.asyncio
async def test_read_file_fails_closed_without_workspace_binding(tmp_path: Path) -> None:
    api_handlers._services_cache_clear()

    with pytest.raises(WorkspaceBindingError, match="workspace binding is missing"):
        await api_handlers.read_file(
            {
                "layer_stack_root": str(tmp_path / "stack"),
                "path": "a.txt",
            }
        )


@pytest.mark.asyncio
async def test_read_file_uses_workspace_base_not_real_workspace(
    tmp_path: Path,
) -> None:
    api_handlers._services_cache_clear()
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    file_path = workspace / "a.txt"
    file_path.write_text("base\n", encoding="utf-8")
    stack = tmp_path / "stack"
    build_workspace_base(workspace_root=workspace, layer_stack_root=stack)
    file_path.write_text("real workspace changed\n", encoding="utf-8")

    result = await api_handlers.read_file(
        {
            "layer_stack_root": str(stack),
            "path": file_path.as_posix(),
        }
    )

    assert result["success"] is True
    assert result["exists"] is True
    assert result["content"] == "base\n"
