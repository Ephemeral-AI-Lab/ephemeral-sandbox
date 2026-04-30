"""Write file tool."""

from __future__ import annotations

from sandbox.code_intelligence.core.types import WriteSpec
from tools.core.base import ToolExecutionContextService, ToolResult
from tools.core.decorator import tool
from tools.core.op_result_to_tool_result import operation_result_to_tool_result
from sandbox.commit import commit_metadata, submit_commit
from tools.daytona_toolkit._mutation_helpers import ci_write_guard
from sandbox.daytona_utils import _get_repo_root, _resolve_path
from tools.daytona_toolkit._file_tool_helpers import (
    WriteFileInput,
    WriteFileOutput,
)


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
    file_path = _resolve_path(file_path, context)
    warnings: list[str] = []

    if guard := ci_write_guard(context, tool_name="write_file", path=file_path):
        return guard

    change = await submit_commit(
        context,
        op="write",
        specs=[WriteSpec(file_path=file_path, content=content, overwrite=True)],
        fallback_paths=[file_path],
        description=f"write {file_path}",
    )

    return operation_result_to_tool_result(
        change.raw,
        tool_name="write_file",
        success_status="written",
        primary_paths=[file_path],
        warnings=warnings,
        success_extra={
            "cwd": _get_repo_root(context) or "",
            "file_path": file_path,
            "bytes_written": len(content.encode("utf-8")),
            "ci_sync": True,
        },
        metadata_extra=commit_metadata(change),
    )


__all__ = ["write_file"]
