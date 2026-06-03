"""lsp.query_symbols - Pyright workspace symbol search."""

from __future__ import annotations

from pydantic import BaseModel, Field

from sandbox._shared.models import Intent
from sandbox.api.plugin_dispatch import call_plugin
from tools._framework.core.base import ToolExecutionContextService, ToolResult
from tools._framework.core.decorator import tool
from tools._framework.core.results import TextToolOutput
from tools.sandbox._lib.tool_context import resolve_tool_sandbox_path


class QuerySymbolsInput(BaseModel):
    query: str = Field(..., description="Symbol name fragment.")
    file_path: str | None = Field(
        default=None,
        description="Optional file path to restrict the search to one document.",
    )


@tool(
    name="lsp.query_symbols",
    description="Return workspace or per-file Python symbol matches for the given query fragment.",
    short_description="LSP query symbols.",
    input_model=QuerySymbolsInput,
    output_model=TextToolOutput,
    intent=Intent.READ_ONLY,
)
async def query_symbols(
    query: str,
    file_path: str | None = None,
    *,
    context: ToolExecutionContextService,
) -> ToolResult:
    payload: dict[str, object] = {"query": query}
    if file_path is not None:
        payload["file_path"] = resolve_tool_sandbox_path(file_path, context)
    return await call_plugin(
        context,
        plugin="lsp",
        op="query_symbols",
        payload=payload,
    )
