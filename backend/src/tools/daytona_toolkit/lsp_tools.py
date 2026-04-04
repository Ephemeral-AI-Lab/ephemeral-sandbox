"""LSP query tools for Daytona sandboxes.

Provides hover, goto-definition, find-references, and diagnostics
via the CodeIntelligenceGateway. All tools degrade gracefully if
no CI service is configured.
"""

from __future__ import annotations

import json
import logging

from pydantic import BaseModel, Field

from ephemeralos.tools.base import BaseTool, ToolExecutionContext, ToolResult
from ephemeralos.tools.daytona_toolkit.ci_integration import get_ci_gateway

logger = logging.getLogger(__name__)


# -- Hover --------------------------------------------------------------------

class DaytonaLspHoverInput(BaseModel):
    file_path: str = Field(description="Path to the file")
    line: int = Field(description="1-based line number")
    character: int = Field(default=0, description="0-based character offset")


class DaytonaLspHoverTool(BaseTool):
    """Get type, signature, and docstring for a symbol without reading the whole file."""

    name = "daytona_lsp_hover"
    description = "Get type information and documentation for a symbol at a position."
    input_model = DaytonaLspHoverInput

    def is_read_only(self, arguments: BaseModel) -> bool:
        return True

    async def execute(
        self, arguments: DaytonaLspHoverInput, context: ToolExecutionContext,
    ) -> ToolResult:
        gw = get_ci_gateway(context)
        if gw is None:
            return ToolResult(output="Code intelligence not available", is_error=True)

        result = gw.hover(arguments.file_path, arguments.line, arguments.character)
        if result is None:
            return ToolResult(output=f"No hover information at {arguments.file_path}:{arguments.line}")

        return ToolResult(
            output=json.dumps({
                "content": result.content,
                "language": result.language,
            }, indent=2),
        )


# -- Goto Definition ----------------------------------------------------------

class DaytonaLspDefinitionInput(BaseModel):
    file_path: str = Field(description="Path to the file")
    line: int = Field(description="1-based line number")
    character: int = Field(default=0, description="0-based character offset")
    symbol: str = Field(default="", description="Symbol name (optional, extracted from position if empty)")


class DaytonaLspDefinitionTool(BaseTool):
    """Jump to the definition of a symbol across files."""

    name = "daytona_lsp_definition"
    description = "Find the definition location of a symbol."
    input_model = DaytonaLspDefinitionInput

    def is_read_only(self, arguments: BaseModel) -> bool:
        return True

    async def execute(
        self, arguments: DaytonaLspDefinitionInput, context: ToolExecutionContext,
    ) -> ToolResult:
        gw = get_ci_gateway(context)
        if gw is None:
            return ToolResult(output="Code intelligence not available", is_error=True)

        results = gw.find_definitions(
            arguments.file_path,
            arguments.symbol,
            arguments.line,
            arguments.character,
        )
        if not results:
            return ToolResult(output=f"No definitions found for symbol at {arguments.file_path}:{arguments.line}")

        defs = []
        for sym in results:
            defs.append({
                "name": sym.name,
                "kind": sym.kind.value if hasattr(sym.kind, "value") else str(sym.kind),
                "file_path": sym.file_path,
                "line": sym.line,
                "character": sym.character,
                "signature": sym.signature,
            })

        return ToolResult(output=json.dumps(defs, indent=2))


# -- Find References ----------------------------------------------------------

class DaytonaLspReferencesInput(BaseModel):
    file_path: str = Field(description="Path to the file")
    line: int = Field(description="1-based line number")
    character: int = Field(default=0, description="0-based character offset")
    symbol: str = Field(default="", description="Symbol name (optional)")


class DaytonaLspReferencesTool(BaseTool):
    """Find all references to a symbol across the codebase."""

    name = "daytona_lsp_references"
    description = "Find all usages/references of a symbol across files."
    input_model = DaytonaLspReferencesInput

    def is_read_only(self, arguments: BaseModel) -> bool:
        return True

    async def execute(
        self, arguments: DaytonaLspReferencesInput, context: ToolExecutionContext,
    ) -> ToolResult:
        gw = get_ci_gateway(context)
        if gw is None:
            return ToolResult(output="Code intelligence not available", is_error=True)

        results = gw.find_references(
            arguments.file_path,
            arguments.symbol,
            arguments.line,
            arguments.character,
        )
        if not results:
            return ToolResult(output=f"No references found at {arguments.file_path}:{arguments.line}")

        refs = []
        for ref in results[:50]:  # Limit output
            refs.append({
                "file_path": ref.file_path,
                "line": ref.line,
                "character": ref.character,
                "text": ref.text,
            })

        output = json.dumps(refs, indent=2)
        if len(results) > 50:
            output += f"\n\n... {len(results)} total references (showing first 50)"

        return ToolResult(output=output)


# -- Diagnostics --------------------------------------------------------------

class DaytonaLspDiagnosticsInput(BaseModel):
    file_path: str = Field(description="Path to the file to check")


class DaytonaLspDiagnosticsTool(BaseTool):
    """Get syntax and semantic diagnostics for a file."""

    name = "daytona_lsp_diagnostics"
    description = "Check a file for syntax errors, type errors, and warnings."
    input_model = DaytonaLspDiagnosticsInput

    def is_read_only(self, arguments: BaseModel) -> bool:
        return True

    async def execute(
        self, arguments: DaytonaLspDiagnosticsInput, context: ToolExecutionContext,
    ) -> ToolResult:
        gw = get_ci_gateway(context)
        if gw is None:
            return ToolResult(output="Code intelligence not available", is_error=True)

        results = gw.diagnostics(arguments.file_path)
        if not results:
            return ToolResult(output=f"No diagnostics for {arguments.file_path} (clean)")

        diags = []
        for d in results:
            diags.append({
                "line": d.line,
                "character": d.character,
                "severity": d.severity.value if hasattr(d.severity, "value") else str(d.severity),
                "message": d.message,
                "source": d.source,
            })

        return ToolResult(output=json.dumps(diags, indent=2))
