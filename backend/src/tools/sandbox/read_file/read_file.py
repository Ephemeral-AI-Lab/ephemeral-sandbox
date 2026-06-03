"""Read file tool."""

from __future__ import annotations

import sandbox.api as sandbox_api
from sandbox._shared.models import Intent
from sandbox.api import ReadFileRequest
from tools._framework.core.base import ToolExecutionContextService, ToolResult
from tools._framework.core.decorator import tool
from tools.sandbox._lib.tool_context import (
    sandbox_audit_kwargs_from_tool_context,
    sandbox_caller_from_tool_context,
    sandbox_path_error_message,
    resolve_tool_sandbox_path,
    sandbox_audit_metadata_from_tool_context,
    sandbox_id_or_missing_error_result,
)
from tools.sandbox._lib.file_payloads import (
    MAX_READ_FILE_LINES,
    ReadFileInput,
    ReadFileOutput,
    build_read_file_result,
)
from .prompt import get_read_file_description


@tool(
    name="read_file",
    description=get_read_file_description(),
    short_description="Read a file from the sandbox.",
    input_model=ReadFileInput,
    output_model=ReadFileOutput,
    intent=Intent.READ_ONLY,
)
async def read_file(
    file_path: str,
    start_line: int = 1,
    end_line: int = MAX_READ_FILE_LINES,
    *,
    context: ToolExecutionContextService,
) -> ToolResult:
    """Read a file."""
    file_path = resolve_tool_sandbox_path(file_path, context)
    sandbox_id, sandbox_id_error = sandbox_id_or_missing_error_result(context)
    if sandbox_id_error is not None:
        return sandbox_id_error
    try:
        result = await sandbox_api.read_file(
            sandbox_id,
            ReadFileRequest(path=file_path, caller=sandbox_caller_from_tool_context(context)),
            **sandbox_audit_kwargs_from_tool_context(context),
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
            timings=result.timings,
            metadata_extra=sandbox_audit_metadata_from_tool_context(context),
        )
    except Exception as exc:
        return ToolResult(
            output=sandbox_path_error_message(exc, file_path) or str(exc),
            is_error=True,
        )


__all__ = ["read_file"]
