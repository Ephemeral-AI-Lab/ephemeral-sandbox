"""Shared runtime for CI query tool implementations."""

from __future__ import annotations

import inspect
import json
import logging
import os
import shlex
import subprocess
from pathlib import Path
from typing import Any

from pydantic import BaseModel, Field

from code_intelligence.core.constants import SUPPORTED_EXTENSIONS, SYMBOL_INDEX_MAX_FILES
from code_intelligence.core.path_utils import relativize_workspace_path
from code_intelligence.core.query_helpers import (
    _build_fallback_specs,
    _dedupe_matches,
    _parse_rg_matches,
    _python_fallback_query_symbols,
)
from code_intelligence.core.types import SymbolKind
from code_intelligence.indexing.file_discovery import (
    collect_local_files,
    collect_remote_files,
)
from tools.core.base import ToolExecutionContextService, ToolResult
from tools.core.ci_runtime import get_ci_service, resolve_sandbox
from tools.core.sandbox_runtime import resolve_daytona_path
from sandbox.daytona_utils import _exec_command

logger = logging.getLogger(__name__)


class CiStatusInput(BaseModel):
    include_edit_hotspots: bool = Field(
        default=True,
        description="Whether to include edit hotspot information.",
    )
    hotspot_limit: int = Field(
        default=10,
        ge=1,
        description="Maximum edit hotspot results when included.",
    )
    hotspot_cross_run: bool = Field(
        default=False,
        description="Query arbiter-backed cross-run contention history.",
    )


class CiStatusOutput(BaseModel):
    status: str | None = Field(
        default=None,
        description="Status string for unavailable code intelligence responses.",
    )
    reason: str | None = Field(
        default=None,
        description="Reason code intelligence is unavailable.",
    )
    sandbox_id: str | None = Field(default=None, description="Sandbox id.")
    initialized: bool | None = Field(
        default=None,
        description="Whether code intelligence is initialized.",
    )
    workspace_root: str | None = Field(default=None, description="Indexed workspace root.")
    symbol_index: dict[str, Any] | None = Field(
        default=None,
        description="Symbol index status payload.",
    )
    arbiter: dict[str, Any] | None = Field(
        default=None,
        description="Edit arbiter status payload.",
    )
    edit_buffer: dict[str, Any] | None = Field(
        default=None,
        description="Edit buffer status payload.",
    )
    lsp: dict[str, Any] | None = Field(default=None, description="LSP status payload.")
    edit_hotspots: dict[str, Any] | None = Field(
        default=None,
        description="Optional edit hotspot payload.",
    )


class CiWorkspaceStructureInput(BaseModel):
    path: str = Field(
        default="",
        description="Subdirectory to list; empty means workspace root.",
    )
    max_depth: int = Field(
        default=3,
        ge=0,
        description="Maximum directory depth to include.",
    )


class CiWorkspaceStructureOutput(BaseModel):
    status: str | None = Field(
        default=None,
        description="Status for unavailable or empty workspace-structure responses.",
    )
    reason: str | None = Field(
        default=None,
        description="Reason workspace structure is unavailable.",
    )
    source: str | None = Field(
        default=None,
        description="Source used for the path list: index, local, remote, or none.",
    )
    path: str = Field(default="", description="Requested path prefix.")
    max_depth: int | None = Field(default=None, description="Requested maximum depth.")
    paths: list[str] = Field(default_factory=list, description="Workspace paths.")
    rendered: str = Field(default="", description="Human-readable newline-delimited paths.")
    message: str | None = Field(default=None, description="Human-readable status message.")


class CiQuerySymbolInput(BaseModel):
    query: str = Field(
        ...,
        description="Symbol name, partial symbol name, or exact file path to search.",
    )
    kind: str = Field(
        default="",
        description="Optional symbol kind filter, such as function, class, method, or variable.",
    )
    references: bool = Field(
        default=False,
        description="Whether to trace callers and import sites for matching definitions.",
    )


class CiSymbolDefinitionOutput(BaseModel):
    name: str = Field(..., description="Symbol name.")
    kind: str = Field(..., description="Symbol kind.")
    file: str = Field(..., description="File containing the symbol.")
    line: int | None = Field(default=None, description="One-based symbol line number.")
    signature: str | None = Field(default=None, description="Symbol signature.")


class CiSymbolReferenceOutput(BaseModel):
    file: str = Field(..., description="Reference file path.")
    line: int | None = Field(default=None, description="One-based reference line number.")
    text: str = Field(default="", description="Reference line text.")


class CiQuerySymbolOutput(BaseModel):
    status: str | None = Field(
        default=None,
        description="Status for unavailable symbol-query responses.",
    )
    reason: str | None = Field(default=None, description="Reason symbol query is unavailable.")
    file: str | None = Field(default=None, description="File path for file-bootstrap queries.")
    definitions: list[CiSymbolDefinitionOutput] = Field(
        default_factory=list,
        description="Matching symbol definitions.",
    )
    references: list[CiSymbolReferenceOutput] = Field(
        default_factory=list,
        description="Reference sites when requested.",
    )
    total_references: int | None = Field(
        default=None,
        description="Total references collected.",
    )
    confidence: str | None = Field(default=None, description="Confidence level for references.")
    reference_status: str | None = Field(
        default=None,
        description="Reference source/status such as lsp or definition_fallback.",
    )
    lsp_reason: str | None = Field(
        default=None,
        description="Reason LSP references were unavailable when using a fallback.",
    )
    hint: str | None = Field(default=None, description="Follow-up guidance.")
    message: str | None = Field(default=None, description="Human-readable status message.")


# -- Query normalization ------------------------------------------------------


def _normalize_symbol_query(query: str) -> str:
    normalized = str(query or "").strip().strip("`'\"")
    for prefix in ("async def ", "def ", "class ", "function "):
        if normalized.startswith(prefix):
            normalized = normalized[len(prefix) :].strip()
            break
    if "(" in normalized:
        normalized = normalized.split("(", 1)[0].strip()
    if normalized.endswith(":"):
        normalized = normalized[:-1].strip()
    return normalized


def _looks_like_file_query(query: str) -> bool:
    candidate = str(query or "").strip()
    if not candidate:
        return False
    if "/" in candidate or "\\" in candidate:
        return True
    lowered = candidate.lower()
    return any(lowered.endswith(ext) for ext in SUPPORTED_EXTENSIONS)


def _record_symbol_navigation(context: ToolExecutionContextService) -> None:
    metadata = getattr(context, "metadata", None)
    if metadata is None:
        return
    current = metadata.get("_ci_symbol_navigation_calls", 0)
    metadata["_ci_symbol_navigation_calls"] = (
        int(current) + 1 if isinstance(current, (int, float)) else 1
    )


# -- Workspace structure ------------------------------------------------------


def _depth_filtered_paths(
    paths: list[str],
    *,
    workspace_root: str,
    path_prefix: str,
    max_depth: int,
) -> list[str]:
    """Filter *paths* (workspace-relative or absolute) to those under *path_prefix* within *max_depth*."""
    normalized_prefix = relativize_workspace_path(path_prefix, workspace_root=workspace_root)
    depth_limit = max(0, int(max_depth))
    rendered: list[str] = []
    for path in paths:
        rel_path = relativize_workspace_path(path, workspace_root=workspace_root)
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


def _workspace_structure_result(
    *,
    path: str,
    max_depth: int,
    source: str,
    paths: list[str] | None = None,
    status: str | None = None,
    reason: str | None = None,
    message: str | None = None,
) -> ToolResult:
    resolved_paths = list(paths or [])
    payload = CiWorkspaceStructureOutput(
        status=status,
        reason=reason,
        source=source,
        path=path,
        max_depth=max_depth,
        paths=resolved_paths,
        rendered="\n".join(resolved_paths) if resolved_paths else "",
        message=message,
    )
    return ToolResult(output=payload.model_dump_json())


def _local_workspace_structure(
    *,
    workspace_root: str,
    path_prefix: str,
    max_depth: int,
) -> list[str] | None:
    root = Path(workspace_root)
    if not root.is_dir():
        return None
    normalized_prefix = relativize_workspace_path(path_prefix, workspace_root=workspace_root)
    start_root = root / normalized_prefix if normalized_prefix else root
    if not start_root.is_dir():
        return []
    files = collect_local_files(start_root, SYMBOL_INDEX_MAX_FILES)
    abs_paths = [str(p) for p in files]
    return _depth_filtered_paths(
        abs_paths,
        workspace_root=workspace_root,
        path_prefix=path_prefix,
        max_depth=max_depth,
    )


async def _remote_workspace_structure(
    context: ToolExecutionContextService,
    *,
    workspace_root: str,
    path_prefix: str,
    max_depth: int,
) -> list[str] | None:
    target = resolve_daytona_path(path_prefix, context) or workspace_root
    if not target:
        return None
    sandbox = await resolve_sandbox(context)
    if sandbox is None:
        return None
    files = collect_remote_files(sandbox, target, SYMBOL_INDEX_MAX_FILES)
    if files is None:
        return None
    return _depth_filtered_paths(
        files,
        workspace_root=workspace_root,
        path_prefix=path_prefix,
        max_depth=max_depth,
    )


# -- Service plumbing ---------------------------------------------------------


def _svc_or_error(context: ToolExecutionContextService) -> tuple[Any | None, ToolResult | None]:
    """Get CI service or return an error ToolResult."""
    svc = get_ci_service(context)
    if svc is None:
        return None, ToolResult(
            output=json.dumps(
                {"status": "unavailable", "reason": "Code intelligence not configured"}
            ),
        )
    return svc, None


# -- Symbol query fallback (rg/python) ----------------------------------------


async def _exec_remote(
    context: ToolExecutionContextService,
    command: str,
    *,
    timeout: int = 30,
    log_label: str,
) -> tuple[Any | None, str]:
    sandbox = await resolve_sandbox(context)
    if sandbox is None:
        return None, ""
    try:
        response = await _exec_command(sandbox, command, timeout=timeout)
    except Exception:
        logger.debug("%s failed", log_label, exc_info=True)
        return None, ""
    return response, (getattr(response, "result", "") or "").strip()


def _run_rg_local(pattern: str, root: str) -> tuple[int, str] | None:
    try:
        response = subprocess.run(
            ["rg", "-n", "--no-heading", "--color", "never", "-e", pattern, root],
            capture_output=True,
            text=True,
            timeout=30,
            check=False,
        )
    except FileNotFoundError:
        return None
    except Exception:
        logger.debug("Local rg query failed for pattern %s", pattern, exc_info=True)
        return None
    return response.returncode, response.stdout


async def _run_rg_remote(
    context: ToolExecutionContextService,
    pattern: str,
    target: str,
    label: str,
) -> tuple[int, str] | None:
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
    collected: list[dict[str, Any]] = []
    for rg_result, spec in zip(rg_results, specs):
        if rg_result is None:
            continue
        exit_code, stdout = rg_result
        if exit_code not in (0, 1) or not stdout:
            continue
        collected.extend(_parse_rg_matches(stdout, query=query, kind=spec.kind))
    return collected


def _local_query_symbols(
    *,
    workspace_root: str,
    query: str,
    kind: str = "",
) -> list[dict[str, Any]] | None:
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


async def _remote_query_symbols(
    context: ToolExecutionContextService,
    *,
    query: str,
    kind: str = "",
) -> list[dict[str, Any]] | None:
    target = resolve_daytona_path("", context)
    specs = _build_fallback_specs(query, kind=kind)
    rg_results = [
        await _run_rg_remote(context, spec.pattern, target, f"Remote symbol query for {query}")
        for spec in specs
    ]
    if any(r is None for r in rg_results):
        return None
    return _dedupe_matches(_fallback_query_symbols(rg_results, specs, query)) or None


# -- Tool: ci_status ----------------------------------------------------------


async def run_ci_status(
    include_edit_hotspots: bool = True,
    hotspot_limit: int = 10,
    hotspot_cross_run: bool = False,
    *,
    context: ToolExecutionContextService,
) -> ToolResult:
    """Check code intelligence service readiness."""
    svc, err = _svc_or_error(context)
    if err:
        return err
    status = svc.status()
    if include_edit_hotspots:
        arbiter = getattr(svc, "arbiter", None)
        run_id = str(context.get("run_id") or "") or None
        if arbiter is not None and hasattr(arbiter, "hotspots_summary"):
            status["edit_hotspots"] = arbiter.hotspots_summary(
                limit=hotspot_limit,
                run_id=run_id,
                cross_run=hotspot_cross_run,
            )
        else:
            status["edit_hotspots"] = {"hotspots": [], "note": "Arbiter history not available"}
    return ToolResult(output=json.dumps(status, indent=2, default=str))


# -- Tool: ci_workspace_structure --------------------------------------------


async def run_ci_workspace_structure(
    path: str = "",
    max_depth: int = 3,
    *,
    context: ToolExecutionContextService,
) -> ToolResult:
    """List workspace file structure."""
    svc, err = _svc_or_error(context)
    if err:
        return err
    si = svc.symbol_index
    if si is None:
        return _workspace_structure_result(
            path=path,
            max_depth=max_depth,
            source="none",
            status="unavailable",
            reason="Symbol index not available",
            message="Symbol index not available",
        )
    workspace_root = str(getattr(svc, "workspace_root", "") or "")

    # Wait for in-flight builds so first-call returns indexed paths.
    if not si.is_built:
        try:
            si.ensure_built(wait=True, timeout=60.0)
        except Exception:
            logger.debug("ci_workspace_structure: symbol index wait failed", exc_info=True)

    if hasattr(si, "paths_with_prefix"):
        indexed_paths = si.paths_with_prefix(path)
    else:
        indexed_paths = []
    paths = _depth_filtered_paths(
        indexed_paths,
        workspace_root=workspace_root,
        path_prefix=path,
        max_depth=max_depth,
    )
    if paths:
        return _workspace_structure_result(
            path=path,
            max_depth=max_depth,
            source="index",
            paths=paths,
        )

    local_paths = _local_workspace_structure(
        workspace_root=workspace_root,
        path_prefix=path,
        max_depth=max_depth,
    )
    if local_paths:
        return _workspace_structure_result(
            path=path,
            max_depth=max_depth,
            source="local",
            paths=local_paths,
        )

    remote_paths = await _remote_workspace_structure(
        context,
        workspace_root=workspace_root,
        path_prefix=path,
        max_depth=max_depth,
    )
    if remote_paths:
        return _workspace_structure_result(
            path=path,
            max_depth=max_depth,
            source="remote",
            paths=remote_paths,
        )

    return _workspace_structure_result(
        path=path,
        max_depth=max_depth,
        source="none",
        status="empty",
        message=(
            "No files indexed yet. Use `glob` for file discovery when the symbol index is cold."
        ),
    )


# -- Tool: ci_query_symbol ----------------------------------------------------


def _reference_definition_priority(workspace_root: str, definition: Any) -> tuple[int, int, int]:
    """Prefer production definitions over test or package-init stubs."""
    fp = str(getattr(definition, "file_path", "") or "").lower()
    basename = os.path.basename(fp)
    is_test = int("/tests/" in fp or basename.startswith("test_") or basename.endswith("_test.py"))
    is_init = int(basename == "__init__.py")
    return (is_test, is_init, len(fp))


def _symbol_leaf_name(name: str) -> str:
    return str(name or "").rsplit(".", 1)[-1]


def _symbol_match_rank(name: str, query: str) -> tuple[int, int, int, str]:
    """Rank symbol matches by exactness, then specificity."""
    lowered_name = str(name or "").strip().lower()
    lowered_query = str(query or "").strip().lower()
    leaf = _symbol_leaf_name(lowered_name)
    exact = lowered_name == lowered_query or leaf == lowered_query
    prefix = lowered_name.startswith(lowered_query) or leaf.startswith(lowered_query)
    contains = lowered_query in lowered_name or lowered_query in leaf
    category = 3
    if exact:
        category = 0
    elif prefix:
        category = 1
    elif contains:
        category = 2
    member_penalty = 1 if "." in str(name or "") else 0
    return (category, member_penalty, len(lowered_name), lowered_name)


def _prioritize_symbol_matches(
    matches: list[Any],
    query: str,
    *,
    get_name: Any,
    get_kind: Any | None = None,
) -> list[Any]:
    lowered_query = str(query or "").strip().lower()
    if not lowered_query or not matches:
        return list(matches)
    ranked = sorted(matches, key=lambda item: _symbol_match_rank(get_name(item), lowered_query))
    exact = [item for item in ranked if _symbol_match_rank(get_name(item), lowered_query)[0] == 0]
    if exact and get_kind is not None:
        structured_exact = [item for item in exact if str(get_kind(item) or "") != "text_match"]
        if structured_exact:
            exact = structured_exact
    return exact or ranked


def _resolve_symbol_column(svc: Any, file_path: str, line: int, symbol_name: str) -> int:
    """Best-effort 0-based column for *symbol_name* on *line* via LSP's line cache."""
    lsp = getattr(svc, "lsp_client", None)
    read_line = getattr(lsp, "_read_line", None)
    if not callable(read_line):
        return 0
    line_text = read_line(file_path, line)
    if line_text is None:
        return 0
    idx = line_text.find(symbol_name)
    return idx if idx >= 0 else 0


def _sandbox_uses_async_exec(sandbox: Any) -> bool:
    process = getattr(sandbox, "process", None)
    exec_fn = getattr(process, "exec", None) if process is not None else None
    return bool(exec_fn) and inspect.iscoroutinefunction(exec_fn)


def _ensure_sync_lsp_sandbox(
    context: ToolExecutionContextService,
    svc: Any,
    lsp: Any,
) -> tuple[Any, str | None]:
    sandbox = getattr(lsp, "_sandbox", None)
    if not _sandbox_uses_async_exec(sandbox):
        return lsp, None

    sandbox_id = str(context.get("sandbox_id") or "").strip()
    if not sandbox_id:
        return lsp, "async_sandbox_lsp_unavailable"

    try:
        from sandbox.service import SandboxService

        sync_sandbox = SandboxService().get_sandbox_object(sandbox_id)
    except Exception:
        logger.debug(
            "Could not resolve sync sandbox handle for LSP reference tracing on %s",
            sandbox_id,
            exc_info=True,
        )
        return lsp, "async_sandbox_lsp_unavailable"

    if _sandbox_uses_async_exec(sync_sandbox):
        return lsp, "async_sandbox_lsp_unavailable"

    rebind = getattr(svc, "rebind_sandbox", None)
    if callable(rebind):
        rebind(sync_sandbox)
        rebound_lsp = getattr(svc, "lsp_client", lsp)
        return rebound_lsp, None

    try:
        lsp._sandbox = sync_sandbox
        reset = getattr(lsp, "reset_backend_availability", None)
        if callable(reset):
            reset()
    except Exception:
        logger.debug("Could not bind sync sandbox handle for LSP", exc_info=True)
        return lsp, "async_sandbox_lsp_unavailable"
    return lsp, None


def _ensure_reference_lsp_ready(
    context: ToolExecutionContextService,
    svc: Any,
) -> tuple[bool | None, str | None]:
    """Best-effort readiness gate before reference tracing."""
    lsp = getattr(svc, "lsp_client", None)
    if lsp is None:
        return False, "lsp_client_missing"

    lsp, sandbox_reason = _ensure_sync_lsp_sandbox(context, svc, lsp)
    if sandbox_reason is not None:
        return False, sandbox_reason

    ensure_ready = getattr(lsp, "ensure_ready", None)
    if not callable(ensure_ready):
        return None, None

    try:
        readiness = ensure_ready(install_missing=True, languages=("python",))
    except Exception as exc:
        logger.debug("LSP readiness probe failed", exc_info=True)
        return False, f"python_backend_probe_failed: {exc}"

    if isinstance(readiness, dict):
        if not readiness.get("python"):
            return False, "python_backend_unavailable"
        return True, None
    return None, None


def _file_query_symbols(
    svc: Any,
    *,
    query: str,
    context: ToolExecutionContextService,
    workspace_root: str,
) -> tuple[str, list[dict[str, Any]]] | None:
    if not _looks_like_file_query(query):
        return None
    resolved = resolve_daytona_path(query, context)
    rel_path = relativize_workspace_path(resolved or query, workspace_root=workspace_root)
    symbol_index = getattr(svc, "symbol_index", None)
    file_symbols = getattr(symbol_index, "file_symbols", None)
    if not rel_path or not callable(file_symbols):
        return rel_path, []

    candidates = [rel_path]
    if not Path(rel_path).suffix:
        candidates.extend([f"{rel_path}.py", f"{rel_path}/__init__.py"])

    matches = []
    matched_path = rel_path
    for candidate in candidates:
        matches = file_symbols(candidate)
        if matches:
            matched_path = candidate
            break

    if not matches and not Path(rel_path).suffix:
        prefix = rel_path.rstrip("/") + "/"
        indexed_paths = (
            symbol_index.indexed_paths()
            if hasattr(symbol_index, "indexed_paths")
            else []
        )
        for indexed_path in indexed_paths:
            indexed_rel = relativize_workspace_path(
                indexed_path,
                workspace_root=workspace_root,
            )
            if not indexed_rel.startswith(prefix):
                continue
            if Path(indexed_rel).suffix.lower() not in SUPPORTED_EXTENSIONS:
                continue
            matches.extend(file_symbols(indexed_path))

    definitions = [
        {
            "name": symbol.name,
            "kind": (symbol.kind.value if hasattr(symbol.kind, "value") else str(symbol.kind)),
            "file": symbol.file_path,
            "line": symbol.line,
            "signature": symbol.signature,
        }
        for symbol in matches
    ]
    return matched_path, definitions


def _kind_value(symbol: Any) -> str:
    kind = getattr(symbol, "kind", None)
    return getattr(kind, "value", None) or str(kind or "")


async def run_ci_query_symbol(
    query: str,
    kind: str = "",
    references: bool = False,
    *,
    context: ToolExecutionContextService,
) -> ToolResult:
    """Search for symbol definitions and optionally trace references."""
    query = _normalize_symbol_query(query)
    svc, err = _svc_or_error(context)
    if err:
        return err

    kind_filter = None
    if kind:
        try:
            kind_filter = SymbolKind(kind.lower())
        except ValueError:
            pass

    workspace_root = str(getattr(svc, "workspace_root", "") or "")
    warmup = getattr(svc, "warmup", None)
    if callable(warmup):
        try:
            warmup()
        except Exception:
            logger.debug("ci warmup failed", exc_info=True)

    file_query = _file_query_symbols(
        svc,
        query=query,
        context=context,
        workspace_root=workspace_root,
    )
    if file_query is not None:
        rel_path, definitions = file_query
        if not definitions:
            return ToolResult(
                output=(
                    f"No indexed symbols found for file '{rel_path or query}'. "
                    "The file may be missing, cold, or have no indexable definitions. "
                    "Use `ci_workspace_structure(...)` to confirm the path, then continue "
                    "with adjacent symbol evidence or report the gap."
                ),
                is_error=True,
            )
        _record_symbol_navigation(context)
        payload: dict[str, Any] = {
            "file": rel_path,
            "definitions": definitions,
            "confidence": "file_symbols",
        }
        if references:
            payload["references"] = []
            payload["total_references"] = 0
            payload["hint"] = (
                "File-path bootstrap query. Use one of the returned symbol names with "
                "`references=true` to trace callers/import sites."
            )
        return ToolResult(output=json.dumps(payload, indent=2))

    _record_symbol_navigation(context)

    results = svc.query_symbols(query)
    if kind_filter:
        results = [s for s in results if s.kind == kind_filter]
    results = _prioritize_symbol_matches(
        results,
        query,
        get_name=lambda s: str(s.name or ""),
        get_kind=_kind_value,
    )

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
        fallback_matches = _prioritize_symbol_matches(
            fallback_matches,
            query,
            get_name=lambda match: str(match.get("name") or ""),
            get_kind=lambda match: str(match.get("kind") or ""),
        )
        if fallback_matches:
            output: dict[str, Any] = {"definitions": fallback_matches}
            if references:
                output["references"] = []
                output["confidence"] = "unavailable"
            return ToolResult(output=json.dumps(output, indent=2))
        payload = CiQuerySymbolOutput(
            definitions=[],
            references=[],
            total_references=0 if references else None,
            confidence="none",
            message=f"No symbols matching '{query}'",
        )
        return ToolResult(output=payload.model_dump_json())

    definitions = [
        {
            "name": s.name,
            "kind": _kind_value(s),
            "file": s.file_path,
            "line": s.line,
            "signature": s.signature,
        }
        for s in results
    ]

    if not references:
        return ToolResult(output=json.dumps({"definitions": definitions}, indent=2))

    # -- Reference tracing via LSP -------------------------------------------

    sorted_defs = sorted(
        results,
        key=lambda d: _reference_definition_priority(workspace_root, d),
    )

    ref_list: list[dict[str, Any]] = []
    used_lsp = False
    lsp_ready, lsp_reason = _ensure_reference_lsp_ready(context, svc)
    if lsp_ready is not False:
        for defn in sorted_defs:
            try:
                col = _resolve_symbol_column(svc, defn.file_path, defn.line, defn.name)
                lsp_refs = svc.find_references(defn.file_path, defn.name, defn.line, col)
                if lsp_refs:
                    used_lsp = True
                    for ref in lsp_refs:
                        ref_list.append(
                            {
                                "file": ref.file_path,
                                "line": ref.line,
                                "text": getattr(ref, "text", ""),
                            }
                        )
            except Exception as exc:
                if lsp_reason is None:
                    lsp_reason = f"find_references_error: {exc}"
                logger.debug("LSP find_references failed for %s", query, exc_info=True)

    if not used_lsp:
        if lsp_reason is None:
            lsp_reason = "no_lsp_references"
        for defn in sorted_defs:
            ref_list.append(
                {
                    "file": defn.file_path,
                    "line": defn.line,
                    "text": f"definition: {_kind_value(defn)} {defn.name}",
                }
            )

    payload = {
        "definitions": definitions,
        "references": ref_list,
        "total_references": len(ref_list),
        "confidence": "full" if used_lsp else "unavailable",
        "reference_status": "lsp" if used_lsp else "definition_fallback",
    }
    if not used_lsp and lsp_reason:
        payload["lsp_reason"] = lsp_reason
    return ToolResult(output=json.dumps(payload, indent=2))


__all__ = [
    "CiQuerySymbolInput",
    "CiQuerySymbolOutput",
    "CiStatusInput",
    "CiStatusOutput",
    "CiWorkspaceStructureInput",
    "CiWorkspaceStructureOutput",
    "run_ci_query_symbol",
    "run_ci_status",
    "run_ci_workspace_structure",
]
