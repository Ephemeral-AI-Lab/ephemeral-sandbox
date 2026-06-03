"""Edit file tool."""

from __future__ import annotations

from sandbox._shared.models import Intent

from pydantic import BaseModel, ConfigDict, Field

import sandbox.api as sandbox_api
from sandbox.api import EditFileRequest, SearchReplaceEdit
from tools._framework.core.base import ToolExecutionContextService, ToolResult
from tools._framework.core.decorator import tool
from tools.sandbox._lib.tool_context import (
    sandbox_audit_kwargs_from_tool_context,
    sandbox_caller_from_tool_context,
    resolve_tool_sandbox_path,
    sandbox_id_or_missing_error_result,
)
from tools.sandbox._lib.mutation_result import project_file_mutation
from .prompt import get_edit_file_description


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
    replace_all: bool = Field(
        default=False,
        description="Replace every occurrence of `old_text` instead of requiring a unique match.",
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
    changed_path_kinds: dict[str, str] = Field(
        default_factory=dict,
        description="Changed paths keyed to write/delete/symlink/opaque_dir.",
    )
    mutation_source: str = Field(default="", description="Mutation source tag.")
    conflict_reason: str | None = Field(default=None, description="Conflict reason when edit failed.")
    error: dict[str, object] = Field(default_factory=dict, description="Typed error payload.")
    applied_edits: int = Field(
        default=0,
        description="Number of edits applied (not occurrence count; one replace_all edit counts as 1).",
    )


def _normalize_edits(
    *,
    old_text: str,
    new_text: str,
    replace_all: bool,
) -> tuple[list[SearchReplaceEdit], str | None]:
    """Convert tool input into one search/replace edit."""
    if not old_text:
        return [], "Provide `old_text` (text to find) and `new_text` (replacement)."
    return [
        SearchReplaceEdit(old_text=old_text, new_text=new_text, replace_all=replace_all)
    ], None


@tool(
    name="edit_file",
    description=get_edit_file_description(),
    short_description="Apply atomic file edits.",
    input_model=EditFileInput,
    output_model=EditFileOutput,
    intent=Intent.WRITE_ALLOWED,
)
async def edit_file(
    file_path: str,
    old_text: str = "",
    new_text: str = "",
    replace_all: bool = False,
    description: str = "",
    *,
    context: ToolExecutionContextService,
) -> ToolResult:
    """Edit a file."""
    file_path = resolve_tool_sandbox_path(file_path, context)

    normalized_edits, edit_error = _normalize_edits(
        old_text=old_text,
        new_text=new_text,
        replace_all=replace_all,
    )
    if edit_error is not None:
        return ToolResult(output=edit_error, is_error=True)

    sandbox_id, sandbox_id_error = sandbox_id_or_missing_error_result(context)
    if sandbox_id_error is not None:
        return sandbox_id_error

    result = await sandbox_api.edit_file(
        sandbox_id,
        EditFileRequest(
            path=file_path,
            edits=tuple(normalized_edits),
            caller=sandbox_caller_from_tool_context(context),
            description=description or f"edit {file_path}",
        ),
        **sandbox_audit_kwargs_from_tool_context(context),
    )

    return project_file_mutation(
        result,
        success_status="edited",
        file_path=file_path,
        success_extra={"applied_edits": result.applied_edits},
        context=context,
    )


__all__ = ["edit_file"]
