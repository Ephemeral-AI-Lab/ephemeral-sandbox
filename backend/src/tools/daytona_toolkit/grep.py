"""Grep tool."""

from __future__ import annotations

import json

from tools.core.base import ToolExecutionContextService, ToolResult
from tools.core.decorator import tool
from sandbox.daytona_utils import (
    _exec_command,
    _extract_exit_code,
    _get_repo_root,
    _path_error,
    _resolve_path,
    _wrap_bash_command,
)
from tools.daytona_toolkit._file_tool_helpers import (
    GrepInput,
    GrepOutput,
    build_find_result,
    run_with_recovery,
)
from tools.daytona_toolkit.search_commands import build_grep_command


@tool(
    name="grep",
    description="Search file contents with a regex and return matching lines.",
    short_description="Search file contents by pattern.",
    input_model=GrepInput,
    output_model=GrepOutput,
)
async def grep(
    pattern: str,
    path: str = ".",
    *,
    context: ToolExecutionContextService,
) -> ToolResult:
    """Search file contents with a regex."""
    cwd = _get_repo_root(context) or ""
    path = _resolve_path(path, context) if path != "." else (cwd or ".")
    try:
        command = _wrap_bash_command(
            build_grep_command(root=path, pattern=pattern)
        )
        response = await run_with_recovery(
            context,
            lambda sandbox: _exec_command(
                sandbox,
                command,
                timeout=60,
            ),
        )
        stdout = getattr(response, "result", "") or ""
        cleaned, exit_code = _extract_exit_code(
            stdout,
            fallback_exit_code=getattr(response, "exit_code", None),
        )
        payload = json.loads(cleaned or "{}")
        if exit_code not in (0, None) or not bool(payload.get("ok", False)):
            return ToolResult(
                output=str(payload.get("error") or cleaned or f"Search failed in {path}"),
                is_error=True,
            )
        raw_matches = payload.get("matches") or []
        matches = [
            item
            for item in raw_matches
            if isinstance(item, dict)
        ]
        return build_find_result(
            cwd=cwd,
            pattern=pattern,
            path=path,
            matches=matches,
            total_matches=payload.get("total_matches"),
        )
    except Exception as exc:
        return ToolResult(
            output=_path_error(exc, path) or str(exc),
            is_error=True,
        )


__all__ = ["grep"]
