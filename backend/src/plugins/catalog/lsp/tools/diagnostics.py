"""lsp.diagnostics - Pyright diagnostics for a Python file."""

from __future__ import annotations

from pydantic import BaseModel, Field

from sandbox._shared.models import Intent
from sandbox.api.plugin_dispatch import call_plugin
from tools._framework.core.base import ToolExecutionContextService, ToolResult
from tools._framework.core.decorator import tool
from tools._framework.core.results import TextToolOutput
from tools.sandbox._lib.tool_context import resolve_tool_sandbox_path


class DiagnosticsInput(BaseModel):
    file_path: str = Field(..., description="Repo-relative or absolute file path.")
    wait_for_diagnostics: bool = Field(
        False,
        description=(
            "When true, wait for at least one Pyright diagnostic before returning, "
            "up to the session diagnostic timeout."
        ),
    )


@tool(
    name="lsp.diagnostics",
    description="Return Pyright diagnostics (errors, warnings, hints) for a Python file.",
    short_description="LSP diagnostics.",
    input_model=DiagnosticsInput,
    output_model=TextToolOutput,
    intent=Intent.READ_ONLY,
)
async def diagnostics(
    file_path: str,
    wait_for_diagnostics: bool = False,
    *,
    context: ToolExecutionContextService,
) -> ToolResult:
    return await call_plugin(
        context,
        plugin="lsp",
        op="diagnostics",
        payload={
            "file_path": resolve_tool_sandbox_path(file_path, context),
            "wait_for_diagnostics": wait_for_diagnostics,
        },
    )
