"""Query-oriented CI tools — read-only code intelligence queries."""

from __future__ import annotations

import json
import logging
from typing import Any

from pydantic import BaseModel, Field

from ephemeralos.tools.base import BaseTool, ToolExecutionContext, ToolResult
from ephemeralos.tools.daytona_toolkit.ci_integration import get_ci_gateway

logger = logging.getLogger(__name__)


def _gw_or_error(context: ToolExecutionContext) -> tuple[Any | None, ToolResult | None]:
    """Get CI gateway or return an error ToolResult."""
    gw = get_ci_gateway(context)
    if gw is None:
        return None, ToolResult(
            output=json.dumps({"status": "unavailable", "reason": "Code intelligence not configured"}),
        )
    return gw, None


# -- CI Status ----------------------------------------------------------------

class CIStatusInput(BaseModel):
    pass


class CIStatusTool(BaseTool):
    """Check code intelligence service readiness."""

    name = "ci_status"
    description = "Check code intelligence readiness: cache, index, LSP, and edit activity."
    input_model = CIStatusInput

    def is_read_only(self, arguments: BaseModel) -> bool:
        return True

    async def execute(self, arguments: CIStatusInput, context: ToolExecutionContext) -> ToolResult:
        gw, err = _gw_or_error(context)
        if err:
            return err
        status = gw.status()
        return ToolResult(output=json.dumps(status, indent=2, default=str))


# -- Workspace Structure ------------------------------------------------------

class WorkspaceStructureInput(BaseModel):
    path: str = Field(default="", description="Subdirectory to list (empty = workspace root)")
    max_depth: int = Field(default=3, ge=1, le=10, description="Maximum directory depth")


class WorkspaceStructureTool(BaseTool):
    """List workspace file structure."""

    name = "ci_workspace_structure"
    description = "List files and directories in the workspace, sorted by path."
    input_model = WorkspaceStructureInput

    def is_read_only(self, arguments: BaseModel) -> bool:
        return True

    async def execute(self, arguments: WorkspaceStructureInput, context: ToolExecutionContext) -> ToolResult:
        gw, err = _gw_or_error(context)
        if err:
            return err

        si = gw.symbol_index
        if si is None:
            return ToolResult(output="Symbol index not available")

        # Get indexed file paths
        from ephemeralos.services.code_intelligence.symbol_index import SymbolIndex
        if isinstance(si, SymbolIndex):
            with si._lock:
                paths = sorted(si._symbols.keys())
        else:
            paths = []

        if arguments.path:
            paths = [p for p in paths if p.startswith(arguments.path)]

        # Limit output
        paths = paths[:500]
        output = "\n".join(paths)
        if len(paths) == 500:
            output += "\n... (truncated at 500 files)"

        return ToolResult(output=output or "No files indexed")


# -- Symbol Query -------------------------------------------------------------

class SymbolQueryInput(BaseModel):
    query: str = Field(description="Symbol name or partial name to search for")
    kind: str = Field(default="", description="Filter by kind: function, class, method, variable")


class SymbolQueryTool(BaseTool):
    """Search for symbols by name."""

    name = "ci_query_symbols"
    description = "Find functions, classes, methods, and variables by name."
    input_model = SymbolQueryInput

    def is_read_only(self, arguments: BaseModel) -> bool:
        return True

    async def execute(self, arguments: SymbolQueryInput, context: ToolExecutionContext) -> ToolResult:
        gw, err = _gw_or_error(context)
        if err:
            return err

        from ephemeralos.services.code_intelligence.types import SymbolKind
        kind = None
        if arguments.kind:
            try:
                kind = SymbolKind(arguments.kind.lower())
            except ValueError:
                pass

        results = gw.query_symbols(arguments.query)
        if kind:
            results = [s for s in results if s.kind == kind]

        if not results:
            return ToolResult(output=f"No symbols matching '{arguments.query}'")

        symbols = []
        for s in results[:100]:
            symbols.append({
                "name": s.name,
                "kind": s.kind.value if hasattr(s.kind, "value") else str(s.kind),
                "file": s.file_path,
                "line": s.line,
                "signature": s.signature,
            })

        return ToolResult(output=json.dumps(symbols, indent=2))


# -- Symbol References --------------------------------------------------------

class SymbolReferencesInput(BaseModel):
    file_path: str = Field(description="File containing the symbol")
    symbol: str = Field(description="Symbol name to find references for")
    line: int = Field(default=0, description="Line number of the symbol")
    character: int = Field(default=0, description="Character offset")


class SymbolReferencesTool(BaseTool):
    """Find all references to a symbol across files."""

    name = "ci_query_references"
    description = "Find all usages of a symbol across the codebase."
    input_model = SymbolReferencesInput

    def is_read_only(self, arguments: BaseModel) -> bool:
        return True

    async def execute(self, arguments: SymbolReferencesInput, context: ToolExecutionContext) -> ToolResult:
        gw, err = _gw_or_error(context)
        if err:
            return err

        results = gw.find_references(
            arguments.file_path, arguments.symbol,
            arguments.line, arguments.character,
        )
        if not results:
            return ToolResult(output=f"No references found for '{arguments.symbol}'")

        refs = []
        for r in results[:50]:
            refs.append({
                "file": r.file_path,
                "line": r.line,
                "text": r.text,
            })

        output = json.dumps(refs, indent=2)
        if len(results) > 50:
            output += f"\n\n... {len(results)} total (showing 50)"

        return ToolResult(output=output)


# -- Edit Hotspots ------------------------------------------------------------

class EditHotspotsInput(BaseModel):
    limit: int = Field(default=10, ge=1, le=50, description="Max results")


class EditHotspotsTool(BaseTool):
    """Find frequently edited / conflict-prone files."""

    name = "ci_edit_hotspots"
    description = "Return files that have been edited most frequently (conflict-prone)."
    input_model = EditHotspotsInput

    def is_read_only(self, arguments: BaseModel) -> bool:
        return True

    async def execute(self, arguments: EditHotspotsInput, context: ToolExecutionContext) -> ToolResult:
        gw, err = _gw_or_error(context)
        if err:
            return err

        arbiter = gw.arbiter
        if arbiter is None:
            return ToolResult(output="Arbiter not available")

        hotspots = arbiter.hotspots(limit=arguments.limit)
        if not hotspots:
            return ToolResult(output="No edit hotspots recorded")

        items = [{"file": fp, "edit_count": count} for fp, count in hotspots]
        return ToolResult(output=json.dumps(items, indent=2))


# -- Recent Changes -----------------------------------------------------------

class RecentChangesInput(BaseModel):
    seconds: float = Field(default=60.0, ge=1, le=3600, description="Look back window in seconds")


class RecentChangesTool(BaseTool):
    """See files changed recently (by other agents or shell commands)."""

    name = "ci_recent_changes"
    description = "List files changed in the last N seconds for change awareness."
    input_model = RecentChangesInput

    def is_read_only(self, arguments: BaseModel) -> bool:
        return True

    async def execute(self, arguments: RecentChangesInput, context: ToolExecutionContext) -> ToolResult:
        gw, err = _gw_or_error(context)
        if err:
            return err

        ledger = gw.ledger
        if ledger is None:
            return ToolResult(output="Ledger not available")

        files = ledger.recent_files(seconds=arguments.seconds)
        if not files:
            return ToolResult(output=f"No files changed in the last {arguments.seconds}s")

        return ToolResult(output=json.dumps(files, indent=2))
