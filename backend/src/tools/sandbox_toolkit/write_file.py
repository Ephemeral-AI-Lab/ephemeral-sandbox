"""Write file tool."""

from __future__ import annotations

from sandbox.api import WriteFileRequest
from sandbox.api.write import write_file as sandbox_write_file
from tools.core.base import ToolExecutionContextService, ToolResult
from tools.core.decorator import tool
from tools.core.sandbox_session import (
    actor_from_context,
    get_repo_root,
    resolve_sandbox_path,
    sandbox_id_or_error,
)
from tools.sandbox_toolkit._file_tool_helpers import (
    WriteFileInput,
    WriteFileOutput,
)
from tools.sandbox_toolkit._mutation_result import mutation_tool_result


@tool(
    name="write_file",
    description=(
        "Create a new file or COMPLETELY OVERWRITE an existing one with UTF-8 text. Atomic via "
        "the commit pipeline. Use only when creating from scratch or intentionally replacing the "
        "whole file. For partial changes use `edit_file`. No append mode. Parent directory must "
        "already exist."
    ),
    short_description="Create or overwrite a file.",
    input_model=WriteFileInput,
    output_model=WriteFileOutput,
)
async def write_file(
    file_path: str,
    content: str,
    *,
    context: ToolExecutionContextService,
) -> ToolResult:
    """Create or overwrite a file."""
    file_path = resolve_sandbox_path(file_path, context)

    sandbox_id, sandbox_id_error = sandbox_id_or_error(context)
    if sandbox_id_error is not None:
        return sandbox_id_error

    result = await sandbox_write_file(
        sandbox_id,
        WriteFileRequest(
            path=file_path,
            content=content,
            actor=actor_from_context(context),
            description=f"write {file_path}",
            overwrite=True,
        ),
    )

    paths = list(result.changed_paths or (file_path,))
    if result.success:
        return mutation_tool_result(
            success=True,
            success_status="written",
            paths=paths,
            success_extra={
                "cwd": get_repo_root(context),
                "file_path": file_path,
                "bytes_written": len(content.encode("utf-8")),
            },
        )

    return mutation_tool_result(
        success=False,
        success_status="written",
        paths=paths,
        failure_status=result.status or None,
        conflict_reason=result.conflict_reason,
    )


__all__ = ["write_file"]
