"""lsp.find_references - Pyright reference search at a cursor."""

from __future__ import annotations

from pydantic import BaseModel, Field

from sandbox._shared.models import Intent
from sandbox.api.plugin_dispatch import call_plugin
from tools._framework.core.base import ToolExecutionContextService, ToolResult
from tools._framework.core.decorator import tool
from tools._framework.core.results import TextToolOutput
from tools.sandbox._lib.tool_context import resolve_tool_sandbox_path


class FindReferencesInput(BaseModel):
    file_path: str = Field(..., description="Repo-relative or absolute file path.")
    line: int = Field(..., ge=0, description="0-based line number.")
    character: int = Field(..., ge=0, description="0-based character offset on the line.")
    include_declaration: bool = Field(
        default=True, description="Include the symbol's own declaration."
    )


@tool(
    name="lsp.find_references",
    description="Return references to the Python symbol at the given cursor.",
    short_description="LSP find references.",
    input_model=FindReferencesInput,
    output_model=TextToolOutput,
    intent=Intent.READ_ONLY,
)
async def find_references(
    file_path: str,
    line: int,
    character: int,
    include_declaration: bool = True,
    *,
    context: ToolExecutionContextService,
) -> ToolResult:
    return await call_plugin(
        context,
        plugin="lsp",
        op="find_references",
        payload={
            "file_path": resolve_tool_sandbox_path(file_path, context),
            "line": line,
            "character": character,
            "include_declaration": include_declaration,
        },
    )
