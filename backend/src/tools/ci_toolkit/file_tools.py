"""File-oriented CI tool — file reads via CI cache."""

from __future__ import annotations

import json
import logging
import os
from pathlib import Path

from code_intelligence.constants import SUPPORTED_EXTENSIONS
from tools.core.base import ToolExecutionContext, ToolResult
from tools.core.ci_runtime import get_ci_service
from tools.core.sandbox_runtime import get_daytona_sandbox, resolve_daytona_path
from tools.core.decorator import tool
from tools.daytona_toolkit._daytona_utils import (
    _normalize_repo_relative_path,
    _normalize_string_list,
)

logger = logging.getLogger(__name__)

_MAX_LINES = 500
_MAX_CHARS = 32_000


def _resolve_ci_file_path(
    path: str,
    *,
    context: ToolExecutionContext,
    workspace_root: str = "",
) -> str:
    if path.startswith("/"):
        return path

    remote_path = resolve_daytona_path(path, context)
    if remote_path.startswith("/"):
        return remote_path

    if workspace_root:
        return os.path.normpath(f"{workspace_root}/{path}")

    base_cwd = str(getattr(context, "cwd", "") or "").strip()
    if base_cwd:
        return os.path.normpath(f"{base_cwd}/{path}")
    return path


def _local_read_candidates(
    path: str,
    *,
    context: ToolExecutionContext,
    resolved_path: str,
) -> list[Path]:
    candidates = [Path(resolved_path)]
    if not path.startswith("/"):
        context_candidate = Path(context.cwd) / path
        if context_candidate not in candidates:
            candidates.append(context_candidate)
    elif Path(path) not in candidates:
        candidates.append(Path(path))
    return candidates


def _is_benchmark_ci_reader(context: ToolExecutionContext) -> bool:
    if not bool(context.metadata.get("team_mode_enabled")):
        return False
    return str(context.metadata.get("agent_name") or "").strip() in {
        "developer",
        "validator",
        "scout",
    }


def _team_scout_source_guard(
    *,
    context: ToolExecutionContext,
    resolved_path: str,
) -> str | None:
    if not bool(context.metadata.get("team_mode_enabled")):
        return None
    if str(context.metadata.get("agent_name") or "").strip() != "scout":
        return None
    if Path(resolved_path).suffix.lower() not in SUPPORTED_EXTENSIONS:
        return None
    return (
        "Scout read guard: team scouts must stay on `ci_query_symbol(...)`, "
        "`ci_workspace_structure(...)`, and `ci_diagnostics(...)` for source "
        "mapping. Keep missing targets missing and report gaps instead of "
        "opening source files with `ci_read_file(...)`."
    )


def _benchmark_ci_read_guard(
    *,
    context: ToolExecutionContext,
    resolved_path: str,
    workspace_root: str,
) -> str | None:
    if not _is_benchmark_ci_reader(context):
        return None
    if Path(resolved_path).suffix.lower() not in SUPPORTED_EXTENSIONS:
        return None
    repo_root = (
        str(workspace_root or "")
        or str(context.metadata.get("daytona_cwd") or "")
        or str(context.cwd or "")
    ).strip()
    benchmark_files = _normalize_string_list(
        context.metadata.get("benchmark_test_files"),
        repo_root,
    )
    if not benchmark_files and not context.metadata.get("benchmark_test_ids"):
        return None
    rel_path = _normalize_repo_relative_path(resolved_path, repo_root) or ""
    if rel_path and rel_path in benchmark_files:
        return (
            "Benchmark read guard: do not open benchmark test files with "
            "`ci_read_file(...)` on coordinated lanes. Keep benchmark tests as "
            "evidence and navigate through CI symbols instead."
        )
    if int(context.metadata.get("_ci_symbol_navigation_calls") or 0) <= 0:
        return (
            "Benchmark read guard: on coordinated benchmark lanes, use "
            "`ci_query_symbol(name)` or `ci_query_symbol(name, references=true)` "
            "before `ci_read_file(...)`. `ci_read_file` is confirmatory only after "
            "CI symbol/reference evidence narrowed the seam."
        )
    return None


@tool(
    name="ci_read_file",
    description=(
        "Read file contents from the workspace sandbox with line numbers. On "
        "coordinated benchmark lanes this is confirmatory only after CI symbol "
        "queries narrowed the seam."
    ),
    short_description="Read a file with line numbers.",
    read_only=True,
)
async def ci_read_file(
    path: str,
    start_line: int = 1,
    max_lines: int = 200,
    *,
    context: ToolExecutionContext,
) -> ToolResult:
    """Read a file from the workspace via CI cache.

    Args:
        path: File path to read
        start_line: First line to read (1-based)
        max_lines: Maximum lines to return

    Returns:
        file_path (str): The file path
        start_line (int): First line returned
        end_line (int): Last line returned
        total_lines (int): Total lines in file
        truncated (bool): Whether file was truncated
        content (str): File contents with line numbers
    """
    svc = get_ci_service(context)
    workspace_root = str(getattr(svc, "workspace_root", "") or "") if svc is not None else ""
    resolved_path = _resolve_ci_file_path(
        path,
        context=context,
        workspace_root=workspace_root,
    )
    scout_guard = _team_scout_source_guard(
        context=context,
        resolved_path=resolved_path,
    )
    if scout_guard:
        return ToolResult(output=scout_guard, is_error=True)
    guard = _benchmark_ci_read_guard(
        context=context,
        resolved_path=resolved_path,
        workspace_root=workspace_root,
    )
    if guard:
        return ToolResult(output=guard, is_error=True)

    sandbox = get_daytona_sandbox(context)
    content = None
    if sandbox is not None:
        try:
            raw = await sandbox.fs.download_file(resolved_path)
            content = raw.decode("utf-8") if isinstance(raw, bytes) else str(raw)
            path = resolved_path
        except UnicodeDecodeError:
            return ToolResult(output=f"Binary file: {resolved_path}", is_error=True)
        except Exception:
            logger.debug("Remote ci_read_file failed for %s", resolved_path, exc_info=True)

    if content is None:
        try:
            for candidate in _local_read_candidates(
                path,
                context=context,
                resolved_path=resolved_path,
            ):
                if candidate.is_file():
                    content = candidate.read_text(encoding="utf-8")
                    path = str(candidate)
                    break
            if content is None:
                return ToolResult(output=f"File not found: {resolved_path}", is_error=True)
        except UnicodeDecodeError:
            return ToolResult(output=f"Binary file: {path}", is_error=True)
        except Exception as exc:
            return ToolResult(output=str(exc), is_error=True)

    lines = content.splitlines()
    total = len(lines)
    start = max(1, start_line)
    requested_end = min(total, start + max_lines - 1)

    selected = []
    rendered_chars = 0
    truncated = False
    end = start - 1
    for i in range(start, requested_end + 1):
        rendered = f"{i:4d}: {lines[i - 1]}"
        extra_chars = len(rendered) + (1 if selected else 0)
        if selected and rendered_chars + extra_chars > _MAX_CHARS:
            truncated = True
            break
        if not selected and len(rendered) > _MAX_CHARS:
            selected.append(rendered[: _MAX_CHARS - 1] + "…")
            rendered_chars = _MAX_CHARS
            truncated = True
            end = i
            break
        selected.append(rendered)
        rendered_chars += extra_chars
        end = i

    result = {
        "file_path": path,
        "start_line": start,
        "end_line": end,
        "total_lines": total,
        "truncated": truncated,
        "content": "\n".join(selected),
    }

    return ToolResult(output=json.dumps(result, indent=2))
