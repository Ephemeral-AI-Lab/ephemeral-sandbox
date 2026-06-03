"""lsp.rename - apply Pyright rename at a cursor."""

from __future__ import annotations

from pydantic import BaseModel, Field

from sandbox._shared.models import Intent
from sandbox.api.plugin_dispatch import call_plugin_write
from tools._framework.core.base import ToolExecutionContextService, ToolResult
from tools._framework.core.decorator import tool
from tools._framework.core.results import TextToolOutput
from tools.sandbox._lib.tool_context import resolve_tool_sandbox_path


class RenameInput(BaseModel):
    file_path: str = Field(..., description="Repo-relative or absolute file path.")
    line: int = Field(..., ge=0, description="0-based line number.")
    character: int = Field(..., ge=0, description="0-based character offset.")
    new_name: str = Field(..., min_length=1, description="Replacement symbol name.")


@tool(
    name="lsp.rename",
    description="Rename a Python symbol with Pyright and publish the workspace edit.",
    short_description="LSP rename.",
    input_model=RenameInput,
    output_model=TextToolOutput,
    intent=Intent.WRITE_ALLOWED,
)
async def rename(
    file_path: str,
    line: int,
    character: int,
    new_name: str,
    *,
    context: ToolExecutionContextService,
) -> ToolResult:
    return await call_plugin_write(
        context,
        plugin="lsp",
        op="rename",
        payload={
            "file_path": resolve_tool_sandbox_path(file_path, context),
            "line": line,
            "character": character,
            "new_name": new_name,
        },
    )
