"""Daytona tool implementations — @tool-decorated functions for sandbox operations."""

from __future__ import annotations

import json
import logging
import shlex
from typing import Any

from tools.core.decorator import tool
from tools.core.base import ToolExecutionContext, ToolResult
from tools.daytona_toolkit._daytona_utils import (
    _truncate,
    _require_sandbox,
    _recover_sandbox,
    _path_error,
    _get_cwd,
    _resolve_path,
    _normalize_repo_relative_path,
    _normalize_string_list,
    _team_repo_write_error,
    _team_repo_write_warning,
    _upload_file_compat,
    is_coordinated_team_agent,
    record_coordination_warning,
)
from tools.core.ci_runtime import (
    abort_ci_write,
    finalize_ci_write,
    prepare_ci_write,
    sync_write_to_ci,
)

logger = logging.getLogger(__name__)

async def _run_with_recovery(
    context: ToolExecutionContext,
    operation: Any,
) -> Any:
    """Run a sandbox operation once, then retry after sandbox recovery."""
    sandbox = await _require_sandbox(context)
    try:
        return await operation(sandbox)
    except Exception as exc:
        return await operation(await _recover_sandbox(context, exc))


def _build_read_file_result(
    *,
    context: ToolExecutionContext,
    file_path: str,
    content: str,
    start_line: int,
    end_line: int | None,
) -> ToolResult:
    lines = content.splitlines()
    total = len(lines)
    start = max(1, start_line)
    end = min(total, end_line) if end_line else total
    selected = [f"{i:4d}: {lines[i - 1]}" for i in range(start, end + 1)]
    return ToolResult(
        output=json.dumps(
            {
                "cwd": _get_cwd(context) or "",
                "file_path": file_path,
                "total_lines": total,
                "start_line": start,
                "end_line": end,
                "content": _truncate("\n".join(selected)),
            }
        )
    )


def _build_match_result(match: Any) -> dict[str, Any]:
    return {
        "file": getattr(match, "file", None) or "",
        "line": getattr(match, "line", None),
        "content": (getattr(match, "content", None) or "").rstrip(),
    }


def _build_write_file_result(
    *,
    context: ToolExecutionContext,
    file_path: str,
    bytes_written: int,
    warning: str | None,
) -> ToolResult:
    return ToolResult(
        output=json.dumps(
            {
                "cwd": _get_cwd(context) or "",
                "file_path": file_path,
                "bytes_written": bytes_written,
                "ci_sync": True,
                "warnings": [warning] if warning else [],
            }
        )
    )


def _build_find_result(
    *,
    cwd: str,
    pattern: str,
    path: str,
    matches: list[Any],
) -> ToolResult:
    return ToolResult(
        output=json.dumps(
            {
                "cwd": cwd,
                "pattern": pattern,
                "path": path,
                "matches": [_build_match_result(match) for match in matches[:500]],
                "total_matches": len(matches),
            }
        )
    )


def _build_glob_result(
    *,
    cwd: str,
    pattern: str,
    path: str,
    files: list[str],
) -> ToolResult:
    return ToolResult(
        output=json.dumps(
            {
                "cwd": cwd,
                "pattern": pattern,
                "path": path,
                "files": files,
                "total_files": len(files),
            }
        )
    )


def _benchmark_read_guard(context: ToolExecutionContext, file_path: str) -> str | None:
    if not is_coordinated_team_agent(context):
        return None
    repo_root = str(_get_cwd(context) or "").strip()
    benchmark_files = _normalize_string_list(
        context.metadata.get("benchmark_test_files"),
        repo_root,
    )
    if not benchmark_files and not context.metadata.get("benchmark_test_ids"):
        return None
    rel_path = _normalize_repo_relative_path(file_path, repo_root) or ""
    if rel_path and rel_path in benchmark_files:
        return (
            "Benchmark read guard: do not open benchmark test files with "
            "`daytona_read_file(...)` on coordinated lanes. Use the named pytest "
            "ids, scout notes, and runtime traceback instead."
        )
    if int(context.metadata.get("_daytona_codeact_calls") or 0) <= 0:
        return (
            "Benchmark read guard: on coordinated benchmark lanes, run the exact "
            "repro first via `daytona_codeact` and direct `shell(\"pytest ...\", "
            "timeout=N)` before using `daytona_read_file(...)`."
        )
    return None


def _build_glob_command(*, root: str, pattern: str) -> str:
    patterns = [pattern]
    if pattern.startswith("**/"):
        patterns.append(pattern[3:])
    payload = json.dumps(list(dict.fromkeys(p for p in patterns if p)))
    script = """
import fnmatch
import json
import os
import sys

root = sys.argv[1]
patterns = json.loads(sys.argv[2])
matches = []

for dirpath, _, filenames in os.walk(root):
    for filename in filenames:
        full_path = os.path.join(dirpath, filename)
        rel_path = os.path.relpath(full_path, root).replace(os.sep, "/")
        if any(
            fnmatch.fnmatch(rel_path, pattern) or fnmatch.fnmatch(filename, pattern)
            for pattern in patterns
        ):
            matches.append(full_path)
            if len(matches) >= 500:
                break
    if len(matches) >= 500:
        break

print("\\n".join(matches))
"""
    return f"python3 -c {shlex.quote(script)} {shlex.quote(root)} {shlex.quote(payload)}"

# ---------------------------------------------------------------------------
# Shell execution
# ---------------------------------------------------------------------------



# ---------------------------------------------------------------------------
# File read
# ---------------------------------------------------------------------------


@tool(
    name="daytona_read_file",
    description=(
        "Read file contents, optionally specifying a line range. On coordinated "
        "benchmark lanes, run the exact runtime repro first and do not use this "
        "to open benchmark test files."
    ),
    short_description="Read a file from the sandbox.",
    read_only=True,
)
async def daytona_read_file(
    file_path: str,
    start_line: int = 1,
    end_line: int | None = None,
    *,
    context: ToolExecutionContext,
) -> ToolResult:
    """Read a file from the Daytona sandbox.

    Args:
        file_path: Path to the file in the sandbox
        start_line: First line to read (1-based)
        end_line: Last line to read (1-based, inclusive)

    Returns:
        file_path (str): Path to the file
        total_lines (int): Total number of lines in the file
        start_line (int): First line returned (1-based)
        end_line (int): Last line returned (1-based)
        content (str): File content with line numbers
    """
    file_path = _resolve_path(file_path, context)
    contract_error = _benchmark_read_guard(context, file_path)
    if contract_error is not None:
        return ToolResult(output=contract_error, is_error=True)
    try:
        raw = await _run_with_recovery(
            context,
            lambda sandbox: sandbox.fs.download_file(file_path),
        )
        content = raw.decode("utf-8") if isinstance(raw, bytes) else str(raw)
        return _build_read_file_result(
            context=context,
            file_path=file_path,
            content=content,
            start_line=start_line,
            end_line=end_line,
        )
    except Exception as exc:
        return ToolResult(
            output=_path_error(exc, file_path) or str(exc),
            is_error=True,
        )


# ---------------------------------------------------------------------------
# File write
# ---------------------------------------------------------------------------


async def _do_raw_write(
    sandbox: Any,
    context: ToolExecutionContext,
    file_path: str,
    content: str,
    content_bytes: bytes,
) -> None:
    """Upload file and sync CI state (no prepared-write path)."""
    await _upload_file_compat(sandbox, content_bytes, file_path)
    sync_write_to_ci(context, file_path, content, edit_type="write", description="daytona_write_file")


@tool(
    name="daytona_write_file",
    description="Create a new file or overwrite an existing file with the given content.",
    short_description="Create or overwrite a file.",
)
async def daytona_write_file(
    file_path: str,
    content: str,
    *,
    context: ToolExecutionContext,
) -> ToolResult:
    """Write/create a file in the Daytona sandbox.

    Args:
        file_path: Path to write in the sandbox
        content: File content to write

    Returns:
        file_path (str): Path that was written
        bytes_written (int): Number of bytes written
    """
    file_path = _resolve_path(file_path, context)
    contract_error = _team_repo_write_error(context, file_path, tool_name="daytona_write_file")
    if contract_error is not None:
        return ToolResult(output=contract_error, is_error=True)
    contract_warning = _team_repo_write_warning(context, file_path, tool_name="daytona_write_file")
    if contract_warning is not None:
        record_coordination_warning(
            context,
            category="write_scope",
            message=contract_warning,
        )
    prepared = None
    content_bytes = content.encode("utf-8")
    strict_occ = is_coordinated_team_agent(context)

    async def _ensure_parent(active_sandbox: Any) -> None:
        parent = "/".join(file_path.split("/")[:-1])
        if parent:
            await active_sandbox.process.exec(f"mkdir -p {shlex.quote(parent)}")

    async def _attempt(active_sandbox: Any) -> ToolResult:
        nonlocal prepared
        prepared, scope_packet, err = prepare_ci_write(
            context, file_path, allow_scope_drift=True,
        )
        if err is not None:
            return ToolResult(
                output=err, is_error=True,
                metadata={"scope_packet": scope_packet, "conflict": True},
            )
        if prepared is not None:
            result = finalize_ci_write(
                context, prepared, content=content,
                edit_type="write", description="daytona_write_file",
            )
            if not getattr(result, "success", False):
                return ToolResult(
                    output=str(getattr(result, "message", "") or "Write failed"),
                    is_error=True,
                    metadata={"conflict": bool(getattr(result, "conflict", False))},
                )
        else:
            if strict_occ:
                return ToolResult(
                    output=(
                        f"daytona_write_file: OCC is unavailable for coordinated write of {file_path}. "
                        "Direct write fallback is disabled in coordinated mode."
                    ),
                    is_error=True,
                    metadata={"conflict": True, "occ_required": True},
                )
            await _ensure_parent(active_sandbox)
            await _do_raw_write(active_sandbox, context, file_path, content, content_bytes)
        return _build_write_file_result(
            context=context, file_path=file_path,
            bytes_written=len(content_bytes), warning=contract_warning,
        )

    try:
        sandbox = await _require_sandbox(context)
        return await _attempt(sandbox)
    except Exception as exc:
        try:
            sandbox = await _recover_sandbox(context, exc)
            return await _attempt(sandbox)
        except Exception as recovery_exc:
            parent = "/".join(file_path.split("/")[:-1])
            return ToolResult(
                output=_path_error(recovery_exc, parent) or str(recovery_exc),
                is_error=True,
            )
    finally:
        abort_ci_write(context, prepared)


# ---------------------------------------------------------------------------
# Grep search
# ---------------------------------------------------------------------------


@tool(
    name="daytona_grep",
    description="Search file contents for a text pattern and return matching lines.",
    short_description="Search file contents by pattern.",
    read_only=True,
)
async def daytona_grep(
    pattern: str,
    path: str = ".",
    *,
    context: ToolExecutionContext,
) -> ToolResult:
    """Search file contents in the Daytona sandbox.

    Args:
        pattern: Text pattern to search for in file contents
        path: File or directory to search

    Returns:
        pattern (str): Pattern that was searched
        path (str): Search root path
        matches (list): Matching results with file, line, content
        total_matches (int): Total matches found
    """
    cwd = _get_cwd(context) or ""
    path = _resolve_path(path, context) if path != "." else (cwd or ".")
    try:
        matches = await _run_with_recovery(
            context,
            lambda sandbox: sandbox.fs.find_files(path, pattern),
        )
        return _build_find_result(cwd=cwd, pattern=pattern, path=path, matches=matches or [])
    except Exception as exc:
        return ToolResult(
            output=_path_error(exc, path) or str(exc),
            is_error=True,
        )


# ---------------------------------------------------------------------------
# Glob search
# ---------------------------------------------------------------------------


@tool(
    name="daytona_glob",
    description="Find files by name using a glob pattern (e.g. '*.py', 'test_*').",
    short_description="Find files by glob.",
    read_only=True,
)
async def daytona_glob(
    pattern: str,
    path: str = ".",
    *,
    context: ToolExecutionContext,
) -> ToolResult:
    """Find files by glob pattern in the Daytona sandbox.

    Args:
        pattern: Glob pattern to match file names (e.g. '*.py', 'test_*')
        path: Root directory to search from

    Returns:
        pattern (str): Glob pattern used
        path (str): Search root path
        files (list): Matching file paths
        total_files (int): Total files found
    """
    cwd = _get_cwd(context) or ""
    path = _resolve_path(path, context) if path != "." else (cwd or ".")
    try:
        command = _build_glob_command(root=path, pattern=pattern)
        resp = await _run_with_recovery(
            context,
            lambda sandbox: sandbox.process.exec(
                command,
                timeout=30,
            ),
        )
        if getattr(resp, "exit_code", 0) not in (0, None):
            return ToolResult(
                output=getattr(resp, "result", "") or f"Glob search failed in {path}",
                is_error=True,
            )
        file_list = [f for f in (resp.result or "").splitlines() if f.strip()][:500]
        return _build_glob_result(cwd=cwd, pattern=pattern, path=path, files=file_list)
    except Exception as exc:
        return ToolResult(
            output=_path_error(exc, path) or str(exc),
            is_error=True,
        )
