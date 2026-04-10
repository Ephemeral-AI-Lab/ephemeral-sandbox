"""Query-oriented CI tools — read-only code intelligence queries."""

from __future__ import annotations

import json
import logging
import re
import shlex
import subprocess
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from code_intelligence.constants import SKIP_DIRECTORIES, SUPPORTED_EXTENSIONS
from tools.core.base import ToolExecutionContext, ToolResult
from tools.daytona_toolkit.ci_integration import (
    build_live_scope_packet,
    get_ci_service,
    get_daytona_sandbox,
    refresh_scope_baseline,
    scope_paths_for_write,
    resolve_daytona_path,
)
from tools.daytona_toolkit.coordination import normalize_scope_paths
from tools.core.decorator import tool

logger = logging.getLogger(__name__)
_SYMBOL_FALLBACK_LIMIT = 100
_REFERENCE_FALLBACK_LIMIT = 100
_IDENTIFIER_RE = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*$")
_PY_DEF_RE = re.compile(r"^\s*(?:async\s+def|def)\s+([A-Za-z_][A-Za-z0-9_]*)\b")
_PY_CLASS_RE = re.compile(r"^\s*class\s+([A-Za-z_][A-Za-z0-9_]*)\b")
_ASSIGN_RE = re.compile(r"^\s*([A-Za-z_][A-Za-z0-9_]*)\s*=")


@dataclass(frozen=True)
class _FallbackSearchSpec:
    pattern: str
    kind: str


def _build_fallback_specs(query: str, *, kind: str = "") -> list[_FallbackSearchSpec]:
    """Return ordered regex specs for definition-first fallback lookup."""
    needle = query.strip()
    if not needle:
        return []

    kind_name = (kind or "").lower().strip()
    exact_identifier = bool(_IDENTIFIER_RE.fullmatch(needle))
    escaped = re.escape(needle)
    specs: list[_FallbackSearchSpec] = []

    allow_functions = kind_name in {"", "function", "method"}
    allow_classes = kind_name in {"", "class"}
    allow_vars = kind_name in {"", "variable", "constant", "property"}

    if exact_identifier and allow_functions:
        specs.extend(
            (
                _FallbackSearchSpec(
                    pattern=rf"^\s*(?:async\s+def|def)\s+{escaped}\b",
                    kind="function",
                ),
                _FallbackSearchSpec(
                    pattern=rf"^\s*(?:async\s+def|def)\s+[A-Za-z_][A-Za-z0-9_]*{escaped}[A-Za-z0-9_]*\b",
                    kind="function",
                ),
            )
        )
    if exact_identifier and allow_classes:
        specs.extend(
            (
                _FallbackSearchSpec(
                    pattern=rf"^\s*class\s+{escaped}\b",
                    kind="class",
                ),
                _FallbackSearchSpec(
                    pattern=rf"^\s*class\s+[A-Za-z_][A-Za-z0-9_]*{escaped}[A-Za-z0-9_]*\b",
                    kind="class",
                ),
            )
        )
    if exact_identifier and allow_vars:
        specs.append(
            _FallbackSearchSpec(
                pattern=rf"^\s*{escaped}\s*=",
                kind="variable",
            )
        )

    boundary = rf"\b{escaped}\b" if exact_identifier else escaped
    specs.append(_FallbackSearchSpec(pattern=boundary, kind="text_match"))
    return specs


def _extract_match_name(snippet: str, *, query: str, kind: str) -> str:
    """Infer the real symbol name from a matched line when possible."""
    if kind == "function":
        match = _PY_DEF_RE.search(snippet)
        if match:
            return match.group(1)
    elif kind == "class":
        match = _PY_CLASS_RE.search(snippet)
        if match:
            return match.group(1)
    elif kind == "variable":
        match = _ASSIGN_RE.search(snippet)
        if match:
            return match.group(1)
    return query


def _dedupe_matches(matches: list[dict[str, Any]]) -> list[dict[str, Any]]:
    def _rank(match: dict[str, Any]) -> tuple[int, int, int, str, int]:
        file_path = str(match.get("file") or "")
        lowered = file_path.lower()
        suffix = Path(file_path).suffix.lower()
        kind = str(match.get("kind") or "")
        is_text_match = 1 if kind == "text_match" else 0
        is_doc_path = 1 if (
            suffix in {".md", ".rst", ".txt"}
            or "/docs/" in lowered
            or lowered.endswith("/history.md")
            or lowered.endswith("/readme.md")
        ) else 0
        depth = file_path.count("/")
        return (is_text_match, is_doc_path, depth, file_path, int(match.get("line") or 0))

    seen: set[tuple[str, int, str, str]] = set()
    deduped: list[dict[str, Any]] = []
    for match in sorted(matches, key=_rank):
        key = (
            str(match.get("file") or ""),
            int(match.get("line") or 0),
            str(match.get("kind") or ""),
            str(match.get("name") or ""),
        )
        if key in seen:
            continue
        seen.add(key)
        deduped.append(match)
        if len(deduped) >= _SYMBOL_FALLBACK_LIMIT:
            break
    return deduped


def _parse_rg_matches(
    output: str,
    *,
    query: str,
    kind: str,
) -> list[dict[str, Any]]:
    matches: list[dict[str, Any]] = []
    for line in output.splitlines():
        parts = line.split(":", 2)
        if len(parts) != 3:
            continue
        file_path, line_no, snippet = parts
        try:
            parsed_line = int(line_no)
        except ValueError:
            parsed_line = 0
        inferred_name = _extract_match_name(snippet, query=query, kind=kind)
        matches.append(
            {
                "name": inferred_name,
                "kind": kind,
                "file": file_path,
                "line": parsed_line,
                "signature": snippet.strip()[:200],
            }
        )
        if len(matches) >= _SYMBOL_FALLBACK_LIMIT:
            break
    return matches


def _build_reference_pattern(symbol: str) -> str:
    needle = symbol.strip()
    if not needle:
        return ""
    escaped = re.escape(needle)
    if _IDENTIFIER_RE.fullmatch(needle):
        return rf"\b{escaped}\b"
    return escaped


def _parse_reference_matches(
    output: str,
    *,
    symbol: str,
    skip_file: str = "",
    skip_line: int = 0,
) -> list[dict[str, Any]]:
    refs: list[dict[str, Any]] = []
    seen: set[tuple[str, int, str]] = set()
    for line in output.splitlines():
        parts = line.split(":", 2)
        if len(parts) != 3:
            continue
        file_path, line_no, snippet = parts
        try:
            parsed_line = int(line_no)
        except ValueError:
            parsed_line = 0
        if skip_file and file_path == skip_file and skip_line and parsed_line == skip_line:
            continue
        key = (file_path, parsed_line, snippet.strip())
        if key in seen:
            continue
        seen.add(key)
        refs.append(
            {
                "file": file_path,
                "line": parsed_line,
                "text": snippet.strip()[:200],
            }
        )
        if len(refs) >= _REFERENCE_FALLBACK_LIMIT:
            break
    return refs


def _local_query_symbols(
    *,
    workspace_root: str,
    query: str,
    kind: str = "",
) -> list[dict[str, Any]] | None:
    """Search the local workspace when the symbol index is cold or incomplete."""
    root = Path(workspace_root)
    if not root.is_dir():
        return None

    collected: list[dict[str, Any]] = []
    for spec in _build_fallback_specs(query, kind=kind):
        try:
            response = subprocess.run(
                [
                    "rg",
                    "-n",
                    "--no-heading",
                    "--color",
                    "never",
                    "-e",
                    spec.pattern,
                    str(root),
                ],
                capture_output=True,
                text=True,
                timeout=30,
                check=False,
            )
        except FileNotFoundError:
            return _python_fallback_query_symbols(root=root, query=query, kind=kind)
        except Exception:
            logger.debug("Local symbol query failed for %s", query, exc_info=True)
            continue
        if response.returncode not in (0, 1):
            continue
        if not response.stdout:
            continue
        collected.extend(
            _parse_rg_matches(response.stdout, query=query, kind=spec.kind)
        )
        if len(collected) >= _SYMBOL_FALLBACK_LIMIT:
            break

    python_matches = _python_fallback_query_symbols(root=root, query=query, kind=kind)
    if python_matches:
        collected.extend(python_matches)
    deduped = _dedupe_matches(collected)
    return deduped or None


def _local_query_references(
    *,
    workspace_root: str,
    symbol: str,
    skip_file: str = "",
    skip_line: int = 0,
) -> list[dict[str, Any]] | None:
    root = Path(workspace_root)
    if not root.is_dir():
        return None

    pattern = _build_reference_pattern(symbol)
    if not pattern:
        return None
    try:
        response = subprocess.run(
            [
                "rg",
                "-n",
                "--no-heading",
                "--color",
                "never",
                "-e",
                pattern,
                str(root),
            ],
            capture_output=True,
            text=True,
            timeout=30,
            check=False,
        )
    except FileNotFoundError:
        return None
    except Exception:
        logger.debug("Local reference query failed for %s", symbol, exc_info=True)
        return None

    if response.returncode not in (0, 1) or not response.stdout:
        return None
    refs = _parse_reference_matches(
        response.stdout,
        symbol=symbol,
        skip_file=skip_file,
        skip_line=skip_line,
    )
    return refs or None


def _python_fallback_query_symbols(
    *,
    root: Path,
    query: str,
    kind: str = "",
) -> list[dict[str, Any]] | None:
    """Last-resort fallback when ripgrep is unavailable."""
    collected: list[dict[str, Any]] = []
    compiled_specs = [
        (re.compile(spec.pattern), spec.kind)
        for spec in _build_fallback_specs(query, kind=kind)
    ]
    if not compiled_specs:
        return None

    for file_path in root.rglob("*"):
        if len(collected) >= _SYMBOL_FALLBACK_LIMIT:
            break
        if not file_path.is_file():
            continue
        if any(part in SKIP_DIRECTORIES for part in file_path.parts):
            continue
        if file_path.suffix.lower() not in SUPPORTED_EXTENSIONS:
            continue
        try:
            lines = file_path.read_text(encoding="utf-8").splitlines()
        except Exception:
            continue
        for lineno, line in enumerate(lines, start=1):
            for pattern, matched_kind in compiled_specs:
                if not pattern.search(line):
                    continue
                collected.append(
                    {
                        "name": _extract_match_name(line, query=query, kind=matched_kind),
                        "kind": matched_kind,
                        "file": str(file_path),
                        "line": lineno,
                        "signature": line.strip()[:200],
                    }
                )
                break
            if len(collected) >= _SYMBOL_FALLBACK_LIMIT:
                break

    deduped = _dedupe_matches(collected)
    return deduped or None


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


async def _remote_query_symbols(
    context: ToolExecutionContext,
    *,
    query: str,
    kind: str = "",
) -> list[dict[str, Any]] | None:
    """Best-effort remote fallback for symbol search on cold starts."""
    sandbox = get_daytona_sandbox(context)
    if sandbox is None:
        return None

    target = resolve_daytona_path("", context)
    collected: list[dict[str, Any]] = []
    for spec in _build_fallback_specs(query, kind=kind):
        command = (
            "rg -n --no-heading --color never "
            f"-e {shlex.quote(spec.pattern)} {shlex.quote(target)}"
        )
        try:
            response = await sandbox.process.exec(command, timeout=30)
        except Exception:
            logger.debug("Remote symbol query failed for %s", query, exc_info=True)
            return None

        exit_code = getattr(response, "exit_code", 0)
        output = (getattr(response, "result", "") or "").strip()
        if exit_code not in (0, 1) or not output:
            continue
        collected.extend(
            _parse_rg_matches(output, query=query, kind=spec.kind)
        )
        if len(collected) >= _SYMBOL_FALLBACK_LIMIT:
            break

    python_matches = await _remote_query_symbols_via_python(
        sandbox=sandbox,
        target=target,
        query=query,
        kind=kind,
    )
    if python_matches:
        collected.extend(python_matches)
    deduped = _dedupe_matches(collected)
    return deduped or None


async def _remote_query_references(
    context: ToolExecutionContext,
    *,
    symbol: str,
    skip_file: str = "",
    skip_line: int = 0,
) -> list[dict[str, Any]] | None:
    sandbox = get_daytona_sandbox(context)
    if sandbox is None:
        return None

    pattern = _build_reference_pattern(symbol)
    if not pattern:
        return None
    target = resolve_daytona_path("", context)
    command = (
        "rg -n --no-heading --color never "
        f"-e {shlex.quote(pattern)} {shlex.quote(target)}"
    )
    try:
        response = await sandbox.process.exec(command, timeout=30)
    except Exception:
        logger.debug("Remote reference query failed for %s", symbol, exc_info=True)
        return None

    exit_code = getattr(response, "exit_code", 0)
    output = (getattr(response, "result", "") or "").strip()
    if exit_code not in (0, 1) or not output:
        return None
    refs = _parse_reference_matches(
        output,
        symbol=symbol,
        skip_file=skip_file,
        skip_line=skip_line,
    )
    return refs or None


async def _remote_query_symbols_via_python(
    *,
    sandbox: Any,
    target: str,
    query: str,
    kind: str = "",
) -> list[dict[str, Any]] | None:
    """Portable remote fallback when ripgrep is unavailable in the sandbox."""
    specs = _build_fallback_specs(query, kind=kind)
    if not specs:
        return None

    payload = json.dumps(
        {
            "root": target,
            "patterns": [{"pattern": spec.pattern, "kind": spec.kind} for spec in specs],
            "skip_dirs": sorted(SKIP_DIRECTORIES),
            "extensions": sorted(SUPPORTED_EXTENSIONS),
            "limit": _SYMBOL_FALLBACK_LIMIT,
        }
    )
    script = """
import json
import os
import re
import sys

payload = json.loads(sys.argv[1])
root = payload["root"]
patterns = [(re.compile(item["pattern"]), item["kind"]) for item in payload["patterns"]]
skip_dirs = set(payload["skip_dirs"])
extensions = tuple(payload["extensions"])
limit = int(payload["limit"])
matches = []

for dirpath, dirnames, filenames in os.walk(root):
    dirnames[:] = [name for name in dirnames if name not in skip_dirs]
    for filename in filenames:
        if not filename.endswith(extensions):
            continue
        path = os.path.join(dirpath, filename)
        try:
            with open(path, encoding="utf-8") as handle:
                for lineno, line in enumerate(handle, start=1):
                    for pattern, match_kind in patterns:
                        if pattern.search(line):
                            matches.append(
                                {
                                    "file": path,
                                    "line": lineno,
                                    "kind": match_kind,
                                    "snippet": line.strip()[:200],
                                }
                            )
                            break
                    if len(matches) >= limit:
                        break
        except Exception:
            continue
        if len(matches) >= limit:
            break
    if len(matches) >= limit:
        break

print(json.dumps(matches))
"""
    command = f"python -c {shlex.quote(script)} {shlex.quote(payload)}"
    try:
        response = await sandbox.process.exec(command, timeout=30)
    except Exception:
        logger.debug("Remote python symbol query failed for %s", query, exc_info=True)
        return None

    exit_code = getattr(response, "exit_code", 0)
    output = (getattr(response, "result", "") or "").strip()
    if exit_code != 0 or not output:
        return None
    try:
        raw_matches = json.loads(output)
    except Exception:
        logger.debug("Remote python symbol query produced invalid JSON for %s", query)
        return None

    collected: list[dict[str, Any]] = []
    for item in raw_matches:
        snippet = str(item.get("snippet") or "")
        matched_kind = str(item.get("kind") or "text_match")
        collected.append(
            {
                "name": _extract_match_name(snippet, query=query, kind=matched_kind),
                "kind": matched_kind,
                "file": str(item.get("file") or ""),
                "line": int(item.get("line") or 0),
                "signature": snippet[:200],
            }
        )
    deduped = _dedupe_matches(collected)
    return deduped or None


# -- CI Status ----------------------------------------------------------------

@tool(name="ci_status", description="Check code intelligence readiness: cache, index, LSP, and edit activity.", read_only=True)
async def ci_status(*, context: ToolExecutionContext) -> ToolResult:
    """Check code intelligence service readiness."""
    svc, err = _svc_or_error(context)
    if err:
        return err
    status = svc.status()
    return ToolResult(output=json.dumps(status, indent=2, default=str))


@tool(
    name="ci_scope_status",
    description=(
        "Return a live scope packet with coherence token, recent changes, "
        "reservations, freshness grade, and scout fanout admission for one or more paths."
    ),
    read_only=True,
)
async def ci_scope_status(
    scope_paths: list[str] | None = None,
    *,
    context: ToolExecutionContext,
) -> ToolResult:
    """Return the current live scope packet for a scope."""
    svc, err = _svc_or_error(context)
    if err:
        return err
    del svc
    requested = normalize_scope_paths(scope_paths or [])
    if not requested:
        requested = normalize_scope_paths(context.metadata.get("default_scope_paths") or [])
    if not requested:
        requested = scope_paths_for_write(context)
    packet = build_live_scope_packet(
        context,
        scope_paths=requested,
    )
    refresh_scope_baseline(context, packet=packet)
    return ToolResult(
        output=json.dumps(packet, indent=2, default=str),
        metadata={
            "scope_packet": packet,
            "coherence_token": str(packet.get("coherence_token") or ""),
        },
    )


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

    workspace_root = str(getattr(svc, "workspace_root", "") or "")
    has_remote_sandbox = get_daytona_sandbox(context) is not None
    should_skip_local_warmup = bool(
        has_remote_sandbox and workspace_root and not Path(workspace_root).is_dir()
    )
    if not getattr(svc, "is_initialized", True) and not should_skip_local_warmup:
        try:
            svc.ensure_initialized(wait=True)
        except Exception:
            logger.debug("ci_query_symbols warmup failed", exc_info=True)

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
                match
                for match in fallback_matches
                if str(match.get("kind") or "") != "text_match"
            ]
        if fallback_matches:
            return ToolResult(output=json.dumps(fallback_matches, indent=2))
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

    workspace_root = str(getattr(svc, "workspace_root", "") or "")
    has_remote_sandbox = get_daytona_sandbox(context) is not None
    should_skip_local_warmup = bool(
        has_remote_sandbox and workspace_root and not Path(workspace_root).is_dir()
    )
    if not getattr(svc, "is_initialized", True) and not should_skip_local_warmup:
        try:
            svc.ensure_initialized(wait=True)
        except Exception:
            logger.debug("ci_query_references warmup failed", exc_info=True)

    results = svc.find_references(
        file_path, symbol,
        line, character,
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
            return ToolResult(output=json.dumps(fallback_refs[:50], indent=2))

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
