"""Edit file tool."""

from __future__ import annotations

import logging

from pydantic import BaseModel, ConfigDict, Field

from sandbox.api.models import EditFileRequest, SearchReplaceEdit
from tools.core.base import ToolExecutionContextService, ToolResult
from tools.core.decorator import tool
from tools.core.sandbox_session import (
    actor_from_context,
    get_repo_root,
    resolve_sandbox_path,
    sandbox_api_or_error,
    sandbox_id_or_error,
)
from tools.sandbox_toolkit._mutation_result import mutation_tool_result

logger = logging.getLogger(__name__)


class EditFileInput(BaseModel):
    model_config = ConfigDict(extra="forbid")

    file_path: str = Field(..., description="Repo-relative or sandbox-root file path.")
    old_text: str = Field(
        default="",
        description="Exact text to replace.",
    )
    new_text: str = Field(
        default="",
        description="Replacement text.",
    )
    description: str = Field(
        default="",
        description="Optional short note about the edit.",
    )


class EditFileOutput(BaseModel):
    cwd: str = Field(..., description="Current sandbox working directory.")
    file_path: str = Field(..., description="Resolved file path that was edited.")
    status: str = Field(..., description="Edit result: edited, aborted_version, or failed.")
    changed_paths: list[str] = Field(default_factory=list, description="Files changed by the edit.")
    conflict_reason: str | None = Field(default=None, description="Conflict reason when edit failed.")
    applied_edits: int = Field(
        default=0,
        description="Number of replacements applied.",
    )


def _normalize_edits(
    *,
    old_text: str,
    new_text: str,
) -> tuple[list[SearchReplaceEdit], str | None]:
    """Convert tool input into one search/replace edit."""
    if not old_text:
        return [], "Provide `old_text` (text to find) and `new_text` (replacement)."
    return [SearchReplaceEdit(old_text=old_text, new_text=new_text)], None


@tool(
    name="edit_file",
    description=(
        "Apply one exact search/replace edit to an existing file. `old_text` must match "
        "byte-for-byte (whitespace, indentation, newlines included) and should be unique — add "
        "surrounding lines if not. Prefer over `write_file` for any modification of an existing "
        "file. Cannot create new files. Returns `aborted_version` if the file changed under you."
    ),
    short_description="Apply atomic file edits.",
    input_model=EditFileInput,
    output_model=EditFileOutput,
)
async def edit_file(
    file_path: str,
    old_text: str = "",
    new_text: str = "",
    description: str = "",
    *,
    context: ToolExecutionContextService,
) -> ToolResult:
    """Edit a file."""
    file_path = resolve_sandbox_path(file_path, context)

    normalized_edits, edit_error = _normalize_edits(
        old_text=old_text,
        new_text=new_text,
    )
    if edit_error is not None:
        return ToolResult(output=edit_error, is_error=True)

    sandbox_id, sandbox_id_error = sandbox_id_or_error(context)
    if sandbox_id_error is not None:
        return sandbox_id_error
    api, api_error = sandbox_api_or_error(context, tool_name="edit_file")
    if api_error is not None:
        return api_error

    result = await api.edit_file(
        sandbox_id,
        EditFileRequest(
            path=file_path,
            edits=tuple(normalized_edits),
            actor=actor_from_context(context),
            description=description or f"edit {file_path}",
        ),
    )

    if not result.success:
        return mutation_tool_result(
            success=False,
            success_status="edited",
            paths=list(result.changed_paths or (file_path,)),
            conflict_reason=result.conflict_reason,
        )

    return mutation_tool_result(
        success=True,
        success_status="edited",
        paths=list(result.changed_paths or (file_path,)),
        success_extra={
            "cwd": get_repo_root(context),
            "file_path": file_path,
            "applied_edits": result.applied_edits,
        },
    )
