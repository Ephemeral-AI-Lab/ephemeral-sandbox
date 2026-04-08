"""Query-oriented CI tools — read-only code intelligence queries."""

from __future__ import annotations

import json
import logging
import shlex
from typing import Any

from tools.core.base import ToolExecutionContext, ToolResult
from tools.daytona_toolkit.ci_integration import (
    get_ci_service,
    get_daytona_sandbox,
    resolve_daytona_path,
)
from tools.core.decorator import tool

logger = logging.getLogger(__name__)


def _svc_or_error(context: ToolExecutionContext) -> tuple[Any | None, ToolResult | None]:
    """Get CI service or return an error ToolResult."""
    svc = get_ci_service(context)
    if svc is None:
        return None, ToolResult(
            output=json.dumps({"status": "unavailable", "reason": "Code intelligence not configured"}),
        )
    return svc, None


async def _remote_workspace_structure(
    context: ToolExecutionContext,
    *,
    path: str,
    max_depth: int,
) -> str | None:
    """List a sandbox-backed workspace when the local symbol index is cold."""
    sandbox = get_daytona_sandbox(context)
    if sandbox is None:
        return None

    target = resolve_daytona_path(path, context)
    command = (
        f"find {shlex.quote(target)} -maxdepth {max(0, int(max_depth))} "
        "-print"
    )
    try:
        response = await sandbox.process.exec(command, timeout=30)
    except Exception:
        logger.debug("Remote workspace listing failed for %s", target, exc_info=True)
        return None

    exit_code = getattr(response, "exit_code", 0)
    if exit_code != 0:
        logger.debug(
            "Remote workspace listing returned exit_code=%s for %s",
            exit_code,
            target,
        )
        return None
    output = (getattr(response, "result", "") or "").strip()
    if not output:
        return None

    lines = sorted(line for line in output.splitlines() if line.strip())
    if not lines:
        return None

    truncated = len(lines) > 500
    rendered = "\n".join(lines[:500])
    if truncated:
        rendered += "\n... (truncated at 500 files)"
    return rendered


# -- CI Status ----------------------------------------------------------------

@tool(name="ci_status", description="Check code intelligence readiness: cache, index, LSP, and edit activity.", read_only=True)
async def ci_status(*, context: ToolExecutionContext) -> ToolResult:
    """Check code intelligence service readiness."""
    svc, err = _svc_or_error(context)
    if err:
        return err
    status = svc.status()
    return ToolResult(output=json.dumps(status, indent=2, default=str))


# -- Workspace Structure ------------------------------------------------------

@tool(name="ci_workspace_structure", description="List files and directories in the workspace, sorted by path.", read_only=True)
async def ci_workspace_structure(
    path: str = "",
    max_depth: int = 3,
    *,
    context: ToolExecutionContext,
) -> ToolResult:
    """List workspace file structure.

    Args:
        path: Subdirectory to list (empty = workspace root)
        max_depth: Maximum directory depth

    Returns:
        output (str): File listing
    """
    svc, err = _svc_or_error(context)
    if err:
        return err

    si = svc.symbol_index
    if si is None:
        return ToolResult(output="Symbol index not available")

    # Get indexed file paths
    from code_intelligence.analysis.symbol_index import SymbolIndex
    if isinstance(si, SymbolIndex):
        with si._lock:
            paths = sorted(si._symbols.keys())
    else:
        paths = []

    if path:
        paths = [p for p in paths if p.startswith(path)]

    # Limit output
    paths = paths[:500]
    output = "\n".join(paths)
    if len(paths) == 500:
        output += "\n... (truncated at 500 files)"

    if output:
        return ToolResult(output=output)

    remote_listing = await _remote_workspace_structure(
        context,
        path=path,
        max_depth=max_depth,
    )
    if remote_listing:
        return ToolResult(output=remote_listing)

    return ToolResult(output="No files indexed")


# -- Symbol Query -------------------------------------------------------------

@tool(name="ci_query_symbols", description="Find functions, classes, methods, and variables by name.", read_only=True)
async def ci_query_symbols(
    query: str,
    kind: str = "",
    *,
    context: ToolExecutionContext,
) -> ToolResult:
    """Search for symbols by name.

    Args:
        query: Symbol name or partial name to search for
        kind: Filter by kind: function, class, method, variable

    Returns:
        symbols (list): Matching symbol entries
    """
    svc, err = _svc_or_error(context)
    if err:
        return err

    from code_intelligence.types import SymbolKind
    kind_filter = None
    if kind:
        try:
            kind_filter = SymbolKind(kind.lower())
        except ValueError:
            pass

    if not getattr(svc, "is_initialized", True):
        try:
            svc.ensure_initialized(wait=True)
        except Exception:
            logger.debug("ci_query_symbols warmup failed", exc_info=True)

    results = svc.query_symbols(query)
    if kind_filter:
        results = [s for s in results if s.kind == kind_filter]

    if not results:
        return ToolResult(output=f"No symbols matching '{query}'")

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

@tool(name="ci_query_references", description="Find all usages of a symbol across the codebase.", read_only=True)
async def ci_query_references(
    file_path: str,
    symbol: str,
    line: int = 0,
    character: int = 0,
    *,
    context: ToolExecutionContext,
) -> ToolResult:
    """Find all references to a symbol across files.

    Args:
        file_path: File containing the symbol
        symbol: Symbol name to find references for
        line: Line number of the symbol
        character: Character offset

    Returns:
        refs (list): Reference locations
    """
    svc, err = _svc_or_error(context)
    if err:
        return err

    results = svc.find_references(
        file_path, symbol,
        line, character,
    )
    if not results:
        return ToolResult(output=f"No references found for '{symbol}'")

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

@tool(name="ci_edit_hotspots", description="Return files that have been edited most frequently (conflict-prone).", read_only=True)
async def ci_edit_hotspots(
    limit: int = 10,
    *,
    context: ToolExecutionContext,
) -> ToolResult:
    """Find frequently edited / conflict-prone files.

    Args:
        limit: Max results

    Returns:
        items (list): Hotspot entries with file and edit_count
    """
    svc, err = _svc_or_error(context)
    if err:
        return err

    arbiter = svc.arbiter
    if arbiter is None:
        return ToolResult(output="Arbiter not available")

    hotspots = arbiter.hotspots(limit=limit)
    if not hotspots:
        return ToolResult(output="No edit hotspots recorded")

    items = [{"file": fp, "edit_count": count} for fp, count in hotspots]
    return ToolResult(output=json.dumps(items, indent=2))


# -- Recent Changes -----------------------------------------------------------

@tool(name="ci_recent_changes", description="List files changed in the last N seconds for change awareness.", read_only=True)
async def ci_recent_changes(
    seconds: float = 60.0,
    *,
    context: ToolExecutionContext,
) -> ToolResult:
    """See files changed recently (by other agents or shell commands).

    Args:
        seconds: Look back window in seconds

    Returns:
        files (list): Recently changed file paths
    """
    svc, err = _svc_or_error(context)
    if err:
        return err

    ledger = svc.ledger
    if ledger is None:
        return ToolResult(output="Ledger not available")

    files = ledger.recent_files(seconds=seconds)
    if not files:
        return ToolResult(output=f"No files changed in the last {seconds}s")

    return ToolResult(output=json.dumps(files, indent=2))
