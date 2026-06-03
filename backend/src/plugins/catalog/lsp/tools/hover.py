"""lsp.hover - Pyright hover info at (file, line, character)."""

from __future__ import annotations

from pydantic import BaseModel, Field

from sandbox._shared.models import Intent
from sandbox.api.plugin_dispatch import call_plugin
from tools._framework.core.base import ToolExecutionContextService, ToolResult
from tools._framework.core.decorator import tool
from tools._framework.core.results import TextToolOutput
from tools.sandbox._lib.tool_context import resolve_tool_sandbox_path


class HoverInput(BaseModel):
    file_path: str = Field(..., description="Repo-relative or absolute file path.")
    line: int = Field(..., ge=0, description="0-based line number.")
    character: int = Field(..., ge=0, description="0-based character offset on the line.")


@tool(
    name="lsp.hover",
    description="Return Pyright hover information for a Python symbol at the given cursor.",
    short_description="LSP hover.",
    input_model=HoverInput,
    output_model=TextToolOutput,
    intent=Intent.READ_ONLY,
)
async def hover(
    file_path: str,
    line: int,
    character: int,
    *,
    context: ToolExecutionContextService,
) -> ToolResult:
    return await call_plugin(
        context,
        plugin="lsp",
        op="hover",
        payload={
            "file_path": resolve_tool_sandbox_path(file_path, context),
            "line": line,
            "character": character,
        },
    )
