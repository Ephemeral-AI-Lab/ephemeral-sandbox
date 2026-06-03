"""Write file tool."""

from __future__ import annotations

import sandbox.api as sandbox_api
from sandbox._shared.models import Intent
from sandbox.api import WriteFileRequest
from tools._framework.core.base import ToolExecutionContextService, ToolResult
from tools._framework.core.decorator import tool
from tools.sandbox._lib.tool_context import (
    sandbox_audit_kwargs_from_tool_context,
    sandbox_caller_from_tool_context,
    resolve_tool_sandbox_path,
    sandbox_id_or_missing_error_result,
)
from tools.sandbox._lib.file_payloads import (
    WriteFileInput,
    WriteFileOutput,
)
from tools.sandbox._lib.mutation_result import project_file_mutation
from .prompt import get_write_file_description


@tool(
    name="write_file",
    description=get_write_file_description(),
    short_description="Create or overwrite a file.",
    input_model=WriteFileInput,
    output_model=WriteFileOutput,
    intent=Intent.WRITE_ALLOWED,
)
async def write_file(
    file_path: str,
    content: str,
    *,
    context: ToolExecutionContextService,
) -> ToolResult:
    """Create or overwrite a file."""
    file_path = resolve_tool_sandbox_path(file_path, context)

    sandbox_id, sandbox_id_error = sandbox_id_or_missing_error_result(context)
    if sandbox_id_error is not None:
        return sandbox_id_error

    result = await sandbox_api.write_file(
        sandbox_id,
        WriteFileRequest(
            path=file_path,
            content=content,
            caller=sandbox_caller_from_tool_context(context),
            description=f"write {file_path}",
            overwrite=True,
        ),
        **sandbox_audit_kwargs_from_tool_context(context),
    )

    return project_file_mutation(
        result,
        success_status="written",
        file_path=file_path,
        success_extra={"bytes_written": len(content.encode("utf-8"))},
        context=context,
    )


__all__ = ["write_file"]
