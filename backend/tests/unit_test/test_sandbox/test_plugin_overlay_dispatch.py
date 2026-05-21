"""Unit tests for automatic plugin operation overlays."""

from __future__ import annotations

from pathlib import Path
from types import SimpleNamespace
from typing import Any

import pytest

from sandbox._shared.models import SandboxCaller
from sandbox.layer_stack.workspace_binding import (
    WorkspaceBinding,
    WorkspaceBindingError,
    write_workspace_binding_atomic,
)
from sandbox.plugin import overlay_dispatch
from sandbox.plugin.op_context import PluginOpContext


class _FakeOverlay:
    def __init__(self, workspace_root: str, scratch_root: Path) -> None:
        self.workspace_root = workspace_root
        self.scratch_root = scratch_root
        self.acquired_workspace_root = ""
        self.published_request = None
        self.published_upperdir = ""
        self.released = False

    def acquire_operation_overlay(
        self,
        *,
        request_id: str,
        workspace_root: str | None = None,
        materialize: bool = False,
    ) -> Any:
        del request_id, materialize
        self.acquired_workspace_root = str(workspace_root or "")
        run_dir = self.scratch_root / "run"
        upperdir = run_dir / "upper"
        workdir = run_dir / "work"
        upperdir.mkdir(parents=True)
        workdir.mkdir()

        def release() -> None:
            self.released = True

        return SimpleNamespace(
            manifest_key="hash@1",
            manifest_version=1,
            root_hash="hash",
            manifest=SimpleNamespace(version=1),
            layer_paths=("/layers/one",),
            run_dir=run_dir.as_posix(),
            upperdir=upperdir.as_posix(),
            workdir=workdir.as_posix(),
            release=release,
        )

    async def publish_cycle(
        self,
        *,
        request: Any,
        upperdir: str,
        snapshot: Any,
        run_maintenance: bool = True,
    ) -> Any:
        del snapshot, run_maintenance
        self.published_request = request
        self.published_upperdir = upperdir
        return SimpleNamespace(
            path_changes=(SimpleNamespace(path="pkg/mod.py"),),
            changeset=SimpleNamespace(
                success=True,
                published_manifest_version=2,
            ),
            timings={"plugin.publish_s": 0.01},
        )


def _write_binding(layer_stack_root: Path, workspace_root: Path) -> None:
    layer_stack_root.mkdir(parents=True)
    workspace_root.mkdir(parents=True)
    write_workspace_binding_atomic(
        WorkspaceBinding(
            workspace_root=workspace_root.as_posix(),
            layer_stack_root=layer_stack_root.as_posix(),
            active_manifest_version=1,
            active_root_hash="active",
            base_manifest_version=1,
            base_root_hash="base",
        )
    )


@pytest.mark.asyncio
async def test_plugin_dispatch_uses_workspace_binding_root(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    workspace_root = tmp_path / "custom-workspace"
    layer_stack_root = tmp_path / "layer-stack"
    _write_binding(layer_stack_root, workspace_root)
    overlay = _FakeOverlay(workspace_root.as_posix(), tmp_path / "scratch")
    ctx = PluginOpContext(
        layer_stack_root=layer_stack_root.as_posix(),
        caller=SandboxCaller(agent_id="agent"),
        projection=SimpleNamespace(),
        overlay=overlay,
        metadata={},
    )

    async def fake_child(**kwargs: Any) -> dict[str, Any]:
        assert kwargs["workspace_root"] == workspace_root.as_posix()
        return {"success": True, "timings": {"plugin.child_s": 0.02}}

    monkeypatch.setattr(overlay_dispatch, "_overlay_namespace_available", lambda: True)
    monkeypatch.setattr(overlay_dispatch, "_run_child_plugin_op", fake_child)

    async def handler(args: dict[str, Any], context: Any) -> dict[str, Any]:
        del args, context
        return {"unused": True}

    result = await overlay_dispatch.run_plugin_op_with_workspace_overlay(
        handler,
        {"value": 1},
        ctx,
        "demo",
        "run",
    )

    assert result["success"] is True
    assert result["timings"] == {
        "plugin.child_s": 0.02,
        "plugin.publish_s": 0.01,
    }
    assert result["plugin_overlay"] == {
        "changed_paths": ["pkg/mod.py"],
        "published_manifest_version": 2,
    }
    assert overlay.acquired_workspace_root == workspace_root.as_posix()
    assert overlay.published_request.workspace_root == workspace_root.as_posix()
    assert overlay.published_upperdir.endswith("/upper")
    assert overlay.released is True


@pytest.mark.asyncio
async def test_plugin_dispatch_rejects_workspace_binding_mismatch(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    workspace_root = tmp_path / "custom-workspace"
    layer_stack_root = tmp_path / "layer-stack"
    _write_binding(layer_stack_root, workspace_root)
    overlay = _FakeOverlay((tmp_path / "other-workspace").as_posix(), tmp_path / "scratch")
    ctx = PluginOpContext(
        layer_stack_root=layer_stack_root.as_posix(),
        caller=SandboxCaller(agent_id="agent"),
        projection=SimpleNamespace(),
        overlay=overlay,
        metadata={},
    )
    monkeypatch.setattr(overlay_dispatch, "_overlay_namespace_available", lambda: True)

    async def handler(args: dict[str, Any], context: Any) -> dict[str, Any]:
        del args, context
        return {"unused": True}

    with pytest.raises(WorkspaceBindingError, match="workspace_root"):
        await overlay_dispatch.run_plugin_op_with_workspace_overlay(
            handler,
            {},
            ctx,
            "demo",
            "run",
        )
