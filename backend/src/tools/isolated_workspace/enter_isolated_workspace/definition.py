"""Agent-facing isolated workspace enter tool."""

from __future__ import annotations

import json

from pydantic import BaseModel

from sandbox._shared.models import EnterIsolatedWorkspaceRequest, Intent
from sandbox.host.isolated_workspace_lifecycle import enter_isolated_workspace as lifecycle_enter
from tools._framework.core.base import (
    TextToolOutput,
    ToolExecutionContextService,
    ToolResult,
)
from tools._framework.core.decorator import tool
from tools._hooks.require_no_inflight_background_tasks import (
    RequireNoInflightBackgroundTasks,
)
from tools.sandbox._lib.tool_context import sandbox_caller_from_tool_context


class EnterIsolatedWorkspaceInput(BaseModel):
    layer_stack_root: str = ""


@tool(
    name="enter_isolated_workspace",
    description="Open a private isolated workspace for this agent.",
    short_description="Enter isolated workspace.",
    input_model=EnterIsolatedWorkspaceInput,
    output_model=TextToolOutput,
    intent=Intent.LIFECYCLE,
    pre_hooks=(RequireNoInflightBackgroundTasks("enter_isolated_workspace"),),
)
async def enter_isolated_workspace(
    layer_stack_root: str = "",
    *,
    context: ToolExecutionContextService,
) -> ToolResult:
    root = layer_stack_root or str(context.get("layer_stack_root") or "")
    result = await lifecycle_enter(
        EnterIsolatedWorkspaceRequest(
            caller=sandbox_caller_from_tool_context(context),
            layer_stack_root=root,
            description="enter isolated workspace",
        ),
        background_manager=context.get("background_task_manager"),
        sandbox_id=str(context.get("sandbox_id") or ""),
    )
    return ToolResult(
        output=json.dumps(_enter_isolated_workspace_payload(result), indent=2),
        is_error=not result.success,
    )


def _enter_isolated_workspace_payload(result: object) -> dict[str, object]:
    error = getattr(result, "error", None)
    return {
        "success": bool(getattr(result, "success", False)),
        "manifest_version": getattr(result, "manifest_version", ""),
        "manifest_root_hash": getattr(result, "manifest_root_hash", ""),
        "error": None if error is None else {
            "kind": error.kind,
            "message": error.message,
            "details": error.details,
        },
    }


__all__ = ["EnterIsolatedWorkspaceInput", "enter_isolated_workspace"]
