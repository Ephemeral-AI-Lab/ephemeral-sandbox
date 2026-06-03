"""lsp.format - format a Python file through Pyright."""

from __future__ import annotations

from typing import Any

from pydantic import BaseModel, Field

from sandbox._shared.models import Intent
from sandbox.api.plugin_dispatch import call_plugin_write
from tools._framework.core.base import ToolExecutionContextService, ToolResult
from tools._framework.core.decorator import tool
from tools._framework.core.results import TextToolOutput
from tools.sandbox._lib.tool_context import resolve_tool_sandbox_path


class FormatInput(BaseModel):
    file_path: str = Field(..., description="Repo-relative or absolute file path.")
    options: dict[str, Any] = Field(
        default_factory=lambda: {"tabSize": 4, "insertSpaces": True},
        description="LSP formatting options.",
    )


@tool(
    name="lsp.format",
    description="Format a Python file through Pyright and publish the edit.",
    short_description="LSP format.",
    input_model=FormatInput,
    output_model=TextToolOutput,
    intent=Intent.WRITE_ALLOWED,
)
async def format_document(
    file_path: str,
    options: dict[str, Any] | None = None,
    *,
    context: ToolExecutionContextService,
) -> ToolResult:
    return await call_plugin_write(
        context,
        plugin="lsp",
        op="format",
        payload={
            "file_path": resolve_tool_sandbox_path(file_path, context),
            "options": options or {"tabSize": 4, "insertSpaces": True},
        },
    )
