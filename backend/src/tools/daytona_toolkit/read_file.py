"""Read file tool."""

from __future__ import annotations

from tools.core.base import ToolExecutionContextService, ToolResult
from tools.core.decorator import tool
from sandbox.daytona_utils import (
    _path_error,
    _read_text_file_via_exec,
    _recover_sandbox,
    _require_sandbox,
    _resolve_path,
)
from tools.daytona_toolkit._file_tool_helpers import (
    ReadFileInput,
    ReadFileOutput,
    build_read_file_result,
)


@tool(
    name="read_file",
    description="Read a sandbox file.",
    short_description="Read a file from the sandbox.",
    input_model=ReadFileInput,
    output_model=ReadFileOutput,
)
async def read_file(
    file_path: str,
    start_line: int = 1,
    end_line: int | None = None,
    *,
    context: ToolExecutionContextService,
) -> ToolResult:
    """Read a file."""
    file_path = _resolve_path(file_path, context)
    try:
        sandbox = await _require_sandbox(context)
        try:
            content, _ = await _read_text_file_via_exec(sandbox, file_path)
        except Exception as exc:
            sandbox = await _recover_sandbox(context, exc)
            content, _ = await _read_text_file_via_exec(sandbox, file_path)
        return build_read_file_result(
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


__all__ = ["read_file"]
