"""Glob tool."""

from __future__ import annotations

from tools.core.base import ToolExecutionContextService, ToolResult
from tools.core.decorator import tool
from tools.daytona_toolkit._daytona_utils import (
    _exec_command,
    _get_repo_root,
    _path_error,
    _resolve_path,
)
from tools.daytona_toolkit._file_tool_helpers import (
    GlobInput,
    GlobOutput,
    _GREP_MATCH_CAP,
    build_glob_result,
    run_with_recovery,
)
from tools.daytona_toolkit.search_commands import build_glob_command


@tool(
    name="glob",
    description="Find files by glob pattern, such as **/*.py or test_*.py.",
    short_description="Find files by glob.",
    input_model=GlobInput,
    output_model=GlobOutput,
)
async def glob(
    pattern: str,
    path: str = ".",
    *,
    context: ToolExecutionContextService,
) -> ToolResult:
    """Find files by glob pattern."""
    cwd = _get_repo_root(context) or ""
    path = _resolve_path(path, context) if path != "." else (cwd or ".")
    try:
        command = build_glob_command(
            root=path,
            pattern=pattern,
            match_cap=_GREP_MATCH_CAP,
        )
        resp = await run_with_recovery(
            context,
            lambda sandbox: _exec_command(
                sandbox,
                command,
                timeout=30,
            ),
        )
        if getattr(resp, "exit_code", 0) not in (0, None):
            return ToolResult(
                output=getattr(resp, "result", "") or f"Glob search failed in {path}",
                is_error=True,
            )
        file_list = [
            f for f in (resp.result or "").splitlines() if f.strip()
        ][: int(_GREP_MATCH_CAP)]
        return build_glob_result(cwd=cwd, pattern=pattern, path=path, files=file_list)
    except Exception as exc:
        return ToolResult(
            output=_path_error(exc, path) or str(exc),
            is_error=True,
        )


__all__ = ["glob"]
