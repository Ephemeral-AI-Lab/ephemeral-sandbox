"""lsp.apply_workspace_edit - apply an LSP WorkspaceEdit."""

from __future__ import annotations

from typing import Any

from pydantic import BaseModel, Field

from sandbox._shared.models import Intent
from sandbox.api.plugin_dispatch import call_plugin_write
from tools._framework.core.base import ToolExecutionContextService, ToolResult
from tools._framework.core.decorator import tool
from tools._framework.core.results import TextToolOutput


class ApplyWorkspaceEditInput(BaseModel):
    edit: dict[str, Any] = Field(..., description="LSP WorkspaceEdit payload.")


@tool(
    name="lsp.apply_workspace_edit",
    description="Apply an LSP WorkspaceEdit to the workspace and publish it.",
    short_description="Apply WorkspaceEdit.",
    input_model=ApplyWorkspaceEditInput,
    output_model=TextToolOutput,
    intent=Intent.WRITE_ALLOWED,
)
async def apply_workspace_edit(
    edit: dict[str, Any],
    *,
    context: ToolExecutionContextService,
) -> ToolResult:
    return await call_plugin_write(
        context,
        plugin="lsp",
        op="apply_workspace_edit",
        payload={"edit": edit},
    )
