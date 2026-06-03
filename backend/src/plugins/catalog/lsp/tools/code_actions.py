"""lsp.code_actions - request Pyright code actions for a range."""

from __future__ import annotations

from typing import Any

from pydantic import BaseModel, Field

from sandbox._shared.models import Intent
from sandbox.api.plugin_dispatch import call_plugin
from tools._framework.core.base import ToolExecutionContextService, ToolResult
from tools._framework.core.decorator import tool
from tools._framework.core.results import TextToolOutput
from tools.sandbox._lib.tool_context import resolve_tool_sandbox_path


class CodeActionsInput(BaseModel):
    file_path: str = Field(..., description="Repo-relative or absolute file path.")
    line: int = Field(0, ge=0, description="0-based line number.")
    character: int = Field(0, ge=0, description="0-based character offset.")
    range: dict[str, Any] | None = Field(None, description="Optional LSP range.")
    diagnostics: list[dict[str, Any]] = Field(default_factory=list)
    only: list[str] | None = Field(None, description="Optional code action kinds.")


@tool(
    name="lsp.code_actions",
    description="Return Pyright code actions for a Python file range.",
    short_description="List code actions.",
    input_model=CodeActionsInput,
    output_model=TextToolOutput,
    intent=Intent.READ_ONLY,
)
async def code_actions(
    file_path: str,
    line: int = 0,
    character: int = 0,
    range: dict[str, Any] | None = None,
    diagnostics: list[dict[str, Any]] | None = None,
    only: list[str] | None = None,
    *,
    context: ToolExecutionContextService,
) -> ToolResult:
    return await call_plugin(
        context,
        plugin="lsp",
        op="code_actions",
        payload={
            "file_path": resolve_tool_sandbox_path(file_path, context),
            "line": line,
            "character": character,
            "range": range,
            "diagnostics": diagnostics or [],
            "only": only,
        },
    )
