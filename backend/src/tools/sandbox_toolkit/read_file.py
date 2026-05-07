"""Read file tool."""

from __future__ import annotations

from sandbox.api import ReadFileRequest, api
from tools.core.base import ToolExecutionContextService, ToolResult
from tools.core.decorator import tool
from tools.sandbox_toolkit.session import (
    caller_from_context,
    path_error,
    resolve_sandbox_path,
    sandbox_id_or_error,
)
from tools.sandbox_toolkit._file_tool_helpers import (
    MAX_READ_FILE_LINES,
    ReadFileInput,
    ReadFileOutput,
    build_read_file_result,
)


@tool(
    name="read_file",
    description=(
        "Read a UTF-8 text file from the sandbox, optionally restricted to a line range. "
        "Each call can return at most 200 lines. Output is line-numbered for easy citation. "
        "Prefer this over `shell` with cat/sed/head — cheaper and structured. Don't use on "
        "binary files or for directory listings (use `glob`). Paths are repo-relative or "
        "sandbox-absolute."
    ),
    short_description="Read a file from the sandbox.",
    input_model=ReadFileInput,
    output_model=ReadFileOutput,
)
async def read_file(
    file_path: str,
    start_line: int = 1,
    end_line: int = MAX_READ_FILE_LINES,
    *,
    context: ToolExecutionContextService,
) -> ToolResult:
    """Read a file."""
    file_path = resolve_sandbox_path(file_path, context)
    sandbox_id, sandbox_id_error = sandbox_id_or_error(context)
    if sandbox_id_error is not None:
        return sandbox_id_error
    try:
        result = await api.read_file(
            sandbox_id,
            ReadFileRequest(path=file_path, caller=caller_from_context(context)),
        )
        if not result.success:
            raise RuntimeError(f"Failed to read file: {file_path}")
        if not result.exists:
            raise FileNotFoundError(file_path)
        return build_read_file_result(
            context=context,
            file_path=file_path,
            content=result.content,
            start_line=start_line,
            end_line=end_line,
        )
    except Exception as exc:
        return ToolResult(
            output=path_error(exc, file_path) or str(exc),
            is_error=True,
        )


__all__ = ["read_file"]
