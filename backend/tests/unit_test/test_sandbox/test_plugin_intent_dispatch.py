"""Plugin/tool intent dispatch contracts."""

from __future__ import annotations

from pathlib import Path
from types import SimpleNamespace
from typing import Any

import pytest
from pydantic import BaseModel

from sandbox._shared.models import Intent, SandboxCaller
from sandbox.ephemeral_workspace.plugin import op_registry, overlay_child, overlay_dispatch
from sandbox.ephemeral_workspace.plugin.op_context import PluginOpContext
from sandbox.ephemeral_workspace.plugin.op_registry import (
    PluginOpRegistrationError,
    flush_plugin_registrations,
    register_plugin_op,
)
from sandbox.layer_stack.workspace_binding import WorkspaceBinding, write_workspace_binding_atomic
from tools._framework.core.decorator import tool
from tools._framework.core.results import ToolResult


@pytest.fixture(autouse=True)
def _clear_registry(monkeypatch: pytest.MonkeyPatch):
    op_registry._PENDING.clear()
    monkeypatch.setattr(op_registry, "_validate_plugin_caller", lambda *_args: None)
    yield
    op_registry._PENDING.clear()


class _Input(BaseModel):
    value: str = ""


class _Output(BaseModel):
    value: str = ""


def test_tool_decorator_requires_intent() -> None:
    with pytest.raises(TypeError, match="@tool requires intent"):

        @tool(input_model=_Input, output_model=_Output)
        async def missing_intent(
            value: str,
            *,
            context,
        ) -> ToolResult:
            del value, context
            return ToolResult(output="unused")


@pytest.mark.asyncio
async def test_read_only_plugin_does_not_allocate_operation_overlay() -> None:
    class _Overlay:
        workspace_root = "/testbed"

        def acquire_operation_overlay(self, **_kwargs):
            raise AssertionError("READ_ONLY plugin op must not allocate overlay")

    async def handler(args: dict[str, Any], ctx: PluginOpContext) -> dict[str, Any]:
        assert ctx.intent is Intent.READ_ONLY
        return {"success": True, "value": args["value"]}

    async def factory(
        _args: dict[str, Any],
        _plugin: str,
        _op: str,
    ) -> PluginOpContext:
        return _context(_Overlay())

    register_plugin_op("demo", "read", intent=Intent.READ_ONLY)(handler)
    registered: dict[str, Any] = {}
    flush_plugin_registrations(
        "demo",
        registered.__setitem__,
        context_factory=factory,
        trusted_caller=True,
    )

    result = await registered["plugin.demo.read"]({"value": 42})

    assert result == {"success": True, "value": 42}


@pytest.mark.asyncio
async def test_write_allowed_plugin_uses_overlay_and_occ(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    layer_stack_root = tmp_path / "stack"
    workspace_root = tmp_path / "workspace"
    _write_binding(layer_stack_root, workspace_root)
    overlay = _WriteOverlay(workspace_root.as_posix(), tmp_path / "scratch")

    async def handler(args: dict[str, Any], ctx: PluginOpContext) -> dict[str, Any]:
        del args, ctx
        raise AssertionError("WRITE_ALLOWED handler should run in child overlay")

    async def fake_child(**_kwargs: Any) -> dict[str, Any]:
        return {"success": True, "timings": {"plugin.child_s": 0.01}}

    monkeypatch.setattr(overlay_dispatch, "_overlay_namespace_available", lambda: True)
    monkeypatch.setattr(overlay_dispatch, "_run_child_plugin_op", fake_child)
    register_plugin_op("demo", "write", intent=Intent.WRITE_ALLOWED)(handler)
    registered: dict[str, Any] = {}

    async def factory(
        _args: dict[str, Any],
        _plugin: str,
        _op: str,
    ) -> PluginOpContext:
        return _context(
            overlay,
            layer_stack_root=layer_stack_root.as_posix(),
        )

    flush_plugin_registrations(
        "demo",
        registered.__setitem__,
        context_factory=factory,
        trusted_caller=True,
    )

    result = await registered["plugin.demo.write"]({"value": 1})

    assert result["success"] is True
    assert result["plugin_overlay"] == {
        "changed_paths": ["pkg/mod.py"],
        "published_manifest_version": 2,
    }
    assert overlay.acquired == 1
    assert overlay.published == 1
    assert overlay.released == 1


def test_lifecycle_intent_rejected_for_plugin_tools() -> None:
    with pytest.raises(PluginOpRegistrationError, match="LIFECYCLE"):
        register_plugin_op("demo", "enter", intent=Intent.LIFECYCLE)


@pytest.mark.asyncio
async def test_overlay_child_preserves_write_intent(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    async def handler(args: dict[str, Any], ctx: PluginOpContext) -> dict[str, Any]:
        assert args == {"value": 1}
        assert ctx.intent is Intent.WRITE_ALLOWED
        return {"success": True}

    monkeypatch.setattr(
        overlay_child,
        "_load_registered_plugin_handler",
        lambda *_args: handler,
    )
    request = SimpleNamespace(
        plugin_name="demo",
        op_name="write",
        args={"value": 1},
        layer_stack_root="/tmp/layer-stack",
        workspace_root=Path("/testbed"),
        manifest_key="root@1",
        manifest_version=1,
        root_hash="root",
        caller={},
        metadata={},
        intent=Intent.WRITE_ALLOWED,
    )

    assert await overlay_child._invoke_registered_plugin_handler(request) == {"success": True}


def _context(
    overlay: Any,
    *,
    layer_stack_root: str = "/tmp/layer-stack",
) -> PluginOpContext:
    return PluginOpContext(
        layer_stack_root=layer_stack_root,
        caller=SandboxCaller(agent_id="agent-a"),
        projection=SimpleNamespace(),
        overlay=overlay,
        intent=Intent.READ_ONLY,
        metadata={},
    )


class _WriteOverlay:
    def __init__(self, workspace_root: str, scratch_root: Path) -> None:
        self.workspace_root = workspace_root
        self._scratch_root = scratch_root
        self.acquired = 0
        self.published = 0
        self.released = 0

    def acquire_operation_overlay(
        self,
        *,
        invocation_id: str,
        workspace_root: str | None = None,
    ) -> Any:
        del invocation_id
        assert workspace_root == self.workspace_root
        self.acquired += 1
        run_dir = self._scratch_root / "run"
        upperdir = run_dir / "upper"
        workdir = run_dir / "work"
        upperdir.mkdir(parents=True)
        workdir.mkdir()

        def release() -> None:
            self.released += 1

        return SimpleNamespace(
            manifest_key="root@1",
            manifest_version=1,
            root_hash="root",
            snapshot_manifest=SimpleNamespace(version=1),
            layer_paths=("/layers/L1",),
            run_dir=run_dir.as_posix(),
            upperdir=upperdir.as_posix(),
            workdir=workdir.as_posix(),
            release=release,
        )

    async def publish_cycle(self, **_kwargs: Any) -> Any:
        self.published += 1
        return SimpleNamespace(
            path_changes=(SimpleNamespace(path="pkg/mod.py"),),
            changeset=SimpleNamespace(success=True, published_manifest_version=2),
            timings={"plugin.publish_s": 0.02},
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
