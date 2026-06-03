"""lsp.apply_code_action - apply the WorkspaceEdit from a code action."""

from __future__ import annotations

from typing import Any

from pydantic import BaseModel, Field

from sandbox._shared.models import Intent
from sandbox.api.plugin_dispatch import call_plugin_write
from tools._framework.core.base import ToolExecutionContextService, ToolResult
from tools._framework.core.decorator import tool
from tools._framework.core.results import TextToolOutput


class ApplyCodeActionInput(BaseModel):
    action: dict[str, Any] = Field(..., description="LSP CodeAction payload.")


@tool(
    name="lsp.apply_code_action",
    description="Apply a Pyright CodeAction edit and publish it.",
    short_description="Apply code action.",
    input_model=ApplyCodeActionInput,
    output_model=TextToolOutput,
    intent=Intent.WRITE_ALLOWED,
)
async def apply_code_action(
    action: dict[str, Any],
    *,
    context: ToolExecutionContextService,
) -> ToolResult:
    return await call_plugin_write(
        context,
        plugin="lsp",
        op="apply_code_action",
        payload={"action": action},
    )
