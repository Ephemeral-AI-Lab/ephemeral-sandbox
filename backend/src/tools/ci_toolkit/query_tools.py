"""Query-oriented CI tools — read-only code intelligence queries."""

from __future__ import annotations

import json
import logging
import os
import shlex
import subprocess
from pathlib import Path
from typing import Any

from code_intelligence.constants import SKIP_DIRECTORIES, SUPPORTED_EXTENSIONS
from code_intelligence.query_helpers import (
    _build_fallback_specs,
    _dedupe_matches,
    _parse_rg_matches,
    _build_reference_pattern,
    _parse_reference_matches,
    _python_fallback_query_symbols,
)
from tools.core.base import ToolExecutionContext, ToolResult
from tools.core.ci_runtime import get_ci_service
from tools.core.sandbox_runtime import get_daytona_sandbox, resolve_daytona_path
from tools.core.decorator import tool

logger = logging.getLogger(__name__)
_SYMBOL_FALLBACK_LIMIT = 100
_REFERENCE_FALLBACK_LIMIT = 100
_STRUCTURE_FALLBACK_LIMIT = 500


def _normalize_workspace_path(path: str, *, workspace_root: str = "") -> str:
    normalized = str(path or "").replace("\\", "/").strip()
    root = str(workspace_root or "").replace("\\", "/").rstrip("/")
    if root and normalized == root:
        return ""
    if root and normalized.startswith(root + "/"):
        normalized = normalized[len(root) + 1 :]
    return normalized.lstrip("./").strip("/")


def _indexed_workspace_paths(
    paths: list[str],
    *,
    workspace_root: str,
    path_prefix: str,
    max_depth: int,
) -> list[str]:
    normalized_prefix = _normalize_workspace_path(path_prefix, workspace_root=workspace_root)
    depth_limit = max(0, int(max_depth))
    rendered: list[str] = []
    for path in paths:
        rel_path = _normalize_workspace_path(path, workspace_root=workspace_root)
        if not rel_path:
            continue
        relative_to_prefix = rel_path
        if normalized_prefix:
            if rel_path == normalized_prefix:
                relative_to_prefix = ""
            elif rel_path.startswith(normalized_prefix + "/"):
                relative_to_prefix = rel_path[len(normalized_prefix) + 1 :]
            else:
                continue
        depth = (
            len([part for part in relative_to_prefix.split("/") if part])
            if relative_to_prefix
            else 0
        )
        if depth <= depth_limit:
            rendered.append(rel_path)
    return rendered


def _reference_result(
    references: list[dict[str, Any]],
    *,
    total_references: int | None = None,
) -> ToolResult:
    total = len(references) if total_references is None else int(total_references)
    return ToolResult(
        output=json.dumps(
            {
                "references": references[:50],
                "total_references": total,
                "truncated": total > 50,
            },
            indent=2,
        )
    )


def _render_workspace_paths(paths: list[str]) -> str:
    output = "\n".join(paths[:_STRUCTURE_FALLBACK_LIMIT])
    if len(paths) > _STRUCTURE_FALLBACK_LIMIT:
        output += "\n... (truncated at 500 files)"
    return output


def _maybe_warm_service(context: ToolExecutionContext, svc: Any, *, label: str) -> None:
    if getattr(svc, "is_initialized", True):
        return
    workspace_root = str(getattr(svc, "workspace_root", "") or "")
    has_remote_sandbox = get_daytona_sandbox(context) is not None
    is_remote_only = bool(
        has_remote_sandbox and workspace_root and not Path(workspace_root).is_dir()
    )
    if is_remote_only:
        # Full ensure_initialized is unsafe for remote-only sandboxes (LSP
        # bootstrap requires a local filesystem).  However the symbol index
        # build runs in its own daemon thread and can safely be awaited.
        si = getattr(svc, "symbol_index", None)
        if si is not None and not getattr(si, "is_built", False):
            try:
                si.ensure_built(wait=True, timeout=60.0)
            except Exception:
                logger.debug(
                    "%s remote symbol index warmup failed", label, exc_info=True
                )
        return
    try:
        svc.ensure_initialized(wait=True)
    except Exception:
        logger.debug("%s warmup failed", label, exc_info=True)


async def _exec_remote(
    context: ToolExecutionContext,
    command: str,
    *,
    timeout: int = 30,
    log_label: str,
) -> tuple[Any | None, str]:
    sandbox = get_daytona_sandbox(context)
    if sandbox is None:
        return None, ""
    try:
        response = await sandbox.process.exec(command, timeout=timeout)
    except Exception:
        logger.debug("%s failed", log_label, exc_info=True)
        return None, ""
    return response, (getattr(response, "result", "") or "").strip()


def _run_rg_local(pattern: str, root: str) -> tuple[int, str] | None:
    """Run ripgrep locally, return (exit_code, stdout) or None on error."""
    try:
        response = subprocess.run(
            ["rg", "-n", "--no-heading", "--color", "never", "-e", pattern, root],
            capture_output=True, text=True, timeout=30, check=False,
        )
    except FileNotFoundError:
        return None
    except Exception:
        logger.debug("Local rg query failed for pattern %s", pattern, exc_info=True)
        return None
    return response.returncode, response.stdout


async def _run_rg_remote(
    context: ToolExecutionContext, pattern: str, target: str, label: str,
) -> tuple[int, str] | None:
    """Run ripgrep on the remote sandbox, return (exit_code, stdout) or None."""
    command = f"rg -n --no-heading --color never -e {shlex.quote(pattern)} {shlex.quote(target)}"
    response, output = await _exec_remote(context, command, log_label=label)
    if response is None:
        return None
    return getattr(response, "exit_code", 0), output


def _fallback_query_symbols(
    rg_results: list[tuple[int, str] | None],
    specs: list[Any],
    query: str,
) -> list[dict[str, Any]]:
    """Parse ripgrep results from multiple fallback specs into symbol matches."""
    collected: list[dict[str, Any]] = []
    for rg_result, spec in zip(rg_results, specs):
        if rg_result is None:
            continue
        exit_code, stdout = rg_result
        if exit_code not in (0, 1) or not stdout:
            continue
        collected.extend(_parse_rg_matches(stdout, query=query, kind=spec.kind))
        if len(collected) >= _SYMBOL_FALLBACK_LIMIT:
            break
    return collected


def _local_query_symbols(
    *, workspace_root: str, query: str, kind: str = "",
) -> list[dict[str, Any]] | None:
    """Search the local workspace when the symbol index is cold or incomplete."""
    root = Path(workspace_root)
    if not root.is_dir():
        return None
    specs = _build_fallback_specs(query, kind=kind)
    rg_results = [_run_rg_local(spec.pattern, str(root)) for spec in specs]
    if any(r is None for r in rg_results):
        py = _python_fallback_query_symbols(root=root, query=query, kind=kind)
        if py:
            return py
    collected = _fallback_query_symbols(rg_results, specs, query)
    python_matches = _python_fallback_query_symbols(root=root, query=query, kind=kind)
    if python_matches:
        collected.extend(python_matches)
    return _dedupe_matches(collected) or None


def _local_query_references(
    *, workspace_root: str, symbol: str, skip_file: str = "", skip_line: int = 0,
) -> list[dict[str, Any]] | None:
    root = Path(workspace_root)
    if not root.is_dir():
        return None
    pattern = _build_reference_pattern(symbol)
    if not pattern:
        return None
    rg_result = _run_rg_local(pattern, str(root))
    if rg_result is None:
        return None
    exit_code, stdout = rg_result
    if exit_code not in (0, 1) or not stdout:
        return None
    return _parse_reference_matches(stdout, symbol=symbol, skip_file=skip_file, skip_line=skip_line) or None


def _local_workspace_structure(
    *,
    workspace_root: str,
    path_prefix: str,
    max_depth: int,
) -> list[str] | None:
    root = Path(workspace_root)
    if not root.is_dir():
        return None

    normalized_prefix = _normalize_workspace_path(path_prefix, workspace_root=workspace_root)
    start_root = root / normalized_prefix if normalized_prefix else root
    if not start_root.is_dir():
        return []

    depth_limit = max(0, int(max_depth))
    collected: list[str] = []
    for dirpath, dirnames, filenames in os.walk(start_root):
        dirnames[:] = [name for name in dirnames if name not in SKIP_DIRECTORIES]

        rel_dir = os.path.relpath(dirpath, start_root)
        dir_depth = 0 if rel_dir == "." else len([part for part in rel_dir.split(os.sep) if part])
        if dir_depth >= depth_limit:
            dirnames[:] = []

        for filename in sorted(filenames):
            if Path(filename).suffix.lower() not in SUPPORTED_EXTENSIONS:
                continue
            full_path = Path(dirpath) / filename
            rel_path = _normalize_workspace_path(str(full_path), workspace_root=workspace_root)
            relative_to_prefix = (
                rel_path[len(normalized_prefix) + 1 :] if normalized_prefix else rel_path
            )
            file_depth = len([part for part in relative_to_prefix.split("/") if part])
            if file_depth <= depth_limit:
                collected.append(rel_path)
            if len(collected) >= _STRUCTURE_FALLBACK_LIMIT:
                return collected
    return collected


async def _remote_workspace_structure(
    context: ToolExecutionContext,
    *,
    workspace_root: str,
    path_prefix: str,
    max_depth: int,
) -> list[str] | None:
    target = resolve_daytona_path(path_prefix, context)
    if not target:
        return None

    script = """
import json
import os
import sys

root = sys.argv[1]
workspace_root = sys.argv[2]
max_depth = int(sys.argv[3])
skip_dirs = set(json.loads(sys.argv[4]))
extensions = set(json.loads(sys.argv[5]))
limit = int(sys.argv[6])

if not os.path.isdir(root):
    sys.exit(0)

matches = []
for dirpath, dirnames, filenames in os.walk(root):
    dirnames[:] = [name for name in dirnames if name not in skip_dirs]

    rel_dir = os.path.relpath(dirpath, root)
    dir_depth = 0 if rel_dir == "." else len([part for part in rel_dir.split(os.sep) if part])
    if dir_depth >= max_depth:
        dirnames[:] = []

    for filename in sorted(filenames):
        if os.path.splitext(filename)[1].lower() not in extensions:
            continue
        full_path = os.path.join(dirpath, filename)
        rel_path = os.path.relpath(full_path, workspace_root).replace(os.sep, "/")
        rel_from_root = os.path.relpath(full_path, root)
        file_depth = len([part for part in rel_from_root.split(os.sep) if part])
        if file_depth <= max_depth:
            matches.append(rel_path)
        if len(matches) >= limit:
            print("\\n".join(matches))
            sys.exit(0)

print("\\n".join(matches))
"""
    command = (
        f"python3 -c {shlex.quote(script)} "
        f"{shlex.quote(target)} "
        f"{shlex.quote(workspace_root)} "
        f"{shlex.quote(str(max(0, int(max_depth))))} "
        f"{shlex.quote(json.dumps(sorted(SKIP_DIRECTORIES)))} "
        f"{shlex.quote(json.dumps(sorted(SUPPORTED_EXTENSIONS)))} "
        f"{shlex.quote(str(_STRUCTURE_FALLBACK_LIMIT))}"
    )
    response, output = await _exec_remote(
        context,
        command,
        log_label=f"Remote workspace structure for {path_prefix or workspace_root}",
    )
    if response is None:
        return None
    if getattr(response, "exit_code", 0) not in (0, 1):
        return None
    return [line.strip() for line in output.splitlines() if line.strip()]


def _svc_or_error(context: ToolExecutionContext) -> tuple[Any | None, ToolResult | None]:
    """Get CI service or return an error ToolResult."""
    svc = get_ci_service(context)
    if svc is None:
        return None, ToolResult(
            output=json.dumps(
                {"status": "unavailable", "reason": "Code intelligence not configured"}
            ),
        )
    return svc, None


async def _remote_query_symbols(
    context: ToolExecutionContext, *, query: str, kind: str = "",
) -> list[dict[str, Any]] | None:
    """Best-effort remote fallback for symbol search on cold starts."""
    target = resolve_daytona_path("", context)
    specs = _build_fallback_specs(query, kind=kind)
    rg_results = [
        await _run_rg_remote(context, spec.pattern, target, f"Remote symbol query for {query}")
        for spec in specs
    ]
    if any(r is None for r in rg_results):
        return None
    return _dedupe_matches(_fallback_query_symbols(rg_results, specs, query)) or None


async def _remote_query_references(
    context: ToolExecutionContext, *, symbol: str, skip_file: str = "", skip_line: int = 0,
) -> list[dict[str, Any]] | None:
    pattern = _build_reference_pattern(symbol)
    if not pattern:
        return None
    target = resolve_daytona_path("", context)
    rg_result = await _run_rg_remote(context, pattern, target, f"Remote ref query for {symbol}")
    if rg_result is None:
        return None
    exit_code, stdout = rg_result
    if exit_code not in (0, 1) or not stdout:
        return None
    return _parse_reference_matches(stdout, symbol=symbol, skip_file=skip_file, skip_line=skip_line) or None


# -- CI Status ----------------------------------------------------------------


@tool(
    name="ci_status",
    description="Check code intelligence readiness: cache, index, LSP, and edit activity.",
    read_only=True,
)
async def ci_status(*, context: ToolExecutionContext) -> ToolResult:
    """Check code intelligence service readiness."""
    svc, err = _svc_or_error(context)
    if err:
        return err
    status = svc.status()
    return ToolResult(output=json.dumps(status, indent=2, default=str))


# -- Workspace Structure ------------------------------------------------------


@tool(
    name="ci_workspace_structure",
    description="List files and directories in the workspace, sorted by path.",
    read_only=True,
)
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
    workspace_root = str(getattr(svc, "workspace_root", "") or "")

    # If the symbol index is still building (e.g. kicked off by
    # inject_code_intelligence for an async sandbox), wait for it so the
    # first ci_workspace_structure call returns indexed paths instead of
    # falling through to the slower remote-listing fallback.
    if not si.is_built:
        try:
            si.ensure_built(wait=True, timeout=60.0)
        except Exception:
            logger.debug("ci_workspace_structure: symbol index wait failed", exc_info=True)

    # Get indexed file paths
    from code_intelligence.analysis.symbol_index import SymbolIndex

    if isinstance(si, SymbolIndex):
        with si._lock:
            paths = sorted(si._symbols.keys())
    else:
        paths = []

    paths = _indexed_workspace_paths(
        paths,
        workspace_root=workspace_root,
        path_prefix=path,
        max_depth=max_depth,
    )

    if paths:
        return ToolResult(output=_render_workspace_paths(paths))

    local_paths = _local_workspace_structure(
        workspace_root=workspace_root,
        path_prefix=path,
        max_depth=max_depth,
    )
    if local_paths:
        return ToolResult(output=_render_workspace_paths(local_paths))

    remote_paths = await _remote_workspace_structure(
        context,
        workspace_root=workspace_root,
        path_prefix=path,
        max_depth=max_depth,
    )
    if remote_paths:
        return ToolResult(output=_render_workspace_paths(remote_paths))

    return ToolResult(
        output="No files indexed yet. Use `daytona_glob` for file discovery when the symbol index is cold."
    )


# -- Symbol Query -------------------------------------------------------------


@tool(
    name="ci_query_symbols",
    description="Find functions, classes, methods, and variables by name.",
    read_only=True,
)
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

    workspace_root = str(getattr(svc, "workspace_root", "") or "")
    _maybe_warm_service(context, svc, label="ci_query_symbols")

    agent_name = str((context.metadata or {}).get("agent_name") or "").strip()
    drop_text_matches = agent_name == "team_planner"

    results = svc.query_symbols(query)
    if kind_filter:
        results = [s for s in results if s.kind == kind_filter]
    if drop_text_matches:
        results = [
            s
            for s in results
            if (
                (getattr(getattr(s, "kind", None), "value", None) or str(getattr(s, "kind", "")))
                != "text_match"
            )
        ]

    if not results:
        fallback_matches: list[dict[str, Any]] = []
        local_matches = _local_query_symbols(
            workspace_root=workspace_root,
            query=query,
            kind=kind,
        )
        if local_matches:
            fallback_matches.extend(local_matches)
        remote_matches = await _remote_query_symbols(context, query=query, kind=kind)
        if remote_matches:
            fallback_matches.extend(remote_matches)
        fallback_matches = _dedupe_matches(fallback_matches)
        if drop_text_matches:
            fallback_matches = [
                match for match in fallback_matches if str(match.get("kind") or "") != "text_match"
            ]
        if fallback_matches:
            return ToolResult(output=json.dumps(fallback_matches, indent=2))
        return ToolResult(output=f"No symbols matching '{query}'")

    symbols = []
    for s in results[:100]:
        symbols.append(
            {
                "name": s.name,
                "kind": s.kind.value if hasattr(s.kind, "value") else str(s.kind),
                "file": s.file_path,
                "line": s.line,
                "signature": s.signature,
            }
        )

    return ToolResult(output=json.dumps(symbols, indent=2))


# -- Symbol References --------------------------------------------------------


@tool(
    name="ci_query_references",
    description="Find all usages of a symbol across the codebase.",
    read_only=True,
)
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

    workspace_root = str(getattr(svc, "workspace_root", "") or "")
    _maybe_warm_service(context, svc, label="ci_query_references")

    results = svc.find_references(
        file_path,
        symbol,
        line,
        character,
    )
    if not results:
        fallback_refs: list[dict[str, Any]] = []
        local_refs = _local_query_references(
            workspace_root=workspace_root,
            symbol=symbol,
            skip_file=file_path,
            skip_line=line,
        )
        if local_refs:
            fallback_refs.extend(local_refs)
        remote_refs = await _remote_query_references(
            context,
            symbol=symbol,
            skip_file=file_path,
            skip_line=line,
        )
        if remote_refs:
            fallback_refs.extend(remote_refs)
        if fallback_refs:
            return _reference_result(
                fallback_refs,
                total_references=len(fallback_refs),
            )

        lsp = getattr(svc, "lsp_client", None)
        lsp_connected = bool(getattr(lsp, "connected", True)) if lsp is not None else True
        if not getattr(svc, "is_initialized", True) or not lsp_connected:
            return ToolResult(
                output=json.dumps(
                    {
                        "status": "cold",
                        "symbol": symbol,
                        "initialized": bool(getattr(svc, "is_initialized", False)),
                        "lsp_connected": lsp_connected,
                        "message": (
                            "Reference search returned no results while code intelligence "
                            "was still warming up or LSP was unavailable."
                        ),
                    },
                    indent=2,
                )
            )
        return ToolResult(output=f"No references found for '{symbol}'")

    refs = []
    for r in results[:50]:
        refs.append(
            {
                "file": r.file_path,
                "line": r.line,
                "text": r.text,
            }
        )

    return _reference_result(refs, total_references=len(results))


# -- Edit Hotspots ------------------------------------------------------------


@tool(
    name="ci_edit_hotspots",
    description="Return files edited most frequently, optionally filtered by scope. Use cross_run for cross-run contention data.",
    read_only=True,
)
async def ci_edit_hotspots(
    limit: int = 10,
    scope_paths: list[str] | None = None,
    cross_run: bool = False,
    *,
    context: ToolExecutionContext,
) -> ToolResult:
    """Find frequently edited / conflict-prone files.

    Args:
        limit: Max results
        scope_paths: Filter to files under these path prefixes
        cross_run: Query cross-run history (FileChangeStore/PG) for multi-agent contention

    Returns:
        hotspots (list): Hotspot entries with file, edit_count, and optionally agents_touched
    """
    svc, err = _svc_or_error(context)
    if err:
        return err

    # Cross-run path: use FileChangeStore for multi-agent contention data
    if cross_run:
        store = context.metadata.get("file_change_store")
        if store is not None and getattr(store, "initialized", False):
            hotspots = store.contention_hotspots(
                scope_prefixes=scope_paths or [],
                limit=limit,
            )
            if hotspots:
                return ToolResult(
                    output=json.dumps(
                        {
                            "hotspots": [
                                {
                                    "file": h.file_path,
                                    "agents_touched": h.agent_count,
                                    "total_edits": h.edit_count,
                                }
                                for h in hotspots
                            ],
                        },
                        indent=2,
                    )
                )
            return ToolResult(
                output=json.dumps(
                    {"hotspots": [], "note": "No cross-run contention history found."}
                )
            )
        return ToolResult(
            output=json.dumps(
                {"hotspots": [], "note": "FileChangeStore not available for cross-run queries."}
            )
        )

    # Same-run path: use arbiter via CI service
    store = getattr(svc.arbiter, "file_change_store", None) if svc.arbiter else None
    if store is None or not getattr(store, "initialized", False):
        return ToolResult(output="FileChangeStore not available")

    # When scope filtering is needed, fetch a larger candidate set so that
    # post-filter doesn't return empty when matching entries exist beyond
    # the initial limit.
    fetch_limit = limit * 5 if scope_paths else limit
    hotspots = store.hotspots(limit=fetch_limit)
    if not hotspots:
        return ToolResult(output="No edit hotspots recorded")

    items = [{"file": fp, "edit_count": count} for fp, count in hotspots]
    if scope_paths:
        items = [
            item
            for item in items
            if any(item["file"].startswith(p.rstrip("/")) for p in scope_paths)
        ]
    return ToolResult(output=json.dumps(items[:limit], indent=2))
