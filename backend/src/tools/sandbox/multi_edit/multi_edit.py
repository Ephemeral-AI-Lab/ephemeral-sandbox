"""Multi-edit tool: apply an ordered batch of edits to one file atomically."""

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
from .prompt import get_multi_edit_description


class MultiEditOp(BaseModel):
    model_config = ConfigDict(extra="forbid")

    old_text: str = Field(..., description="Exact text to find.")
    new_text: str = Field(default="", description="Replacement text.")
    replace_all: bool = Field(
        default=False,
        description="Replace every occurrence of `old_text` instead of requiring a unique match.",
    )


class MultiEditInput(BaseModel):
    model_config = ConfigDict(extra="forbid")

    file_path: str = Field(..., description="Repo-relative or sandbox-root file path.")
    edits: list[MultiEditOp] = Field(
        ...,
        description=(
            "Ordered edits applied sequentially against evolving content "
            "(edit N sees edit N-1's result); all-or-nothing."
        ),
    )
    description: str = Field(
        default="",
        description="Optional short note about the edits.",
    )


class MultiEditOutput(BaseModel):
    cwd: str = Field(..., description="Current sandbox working directory.")
    file_path: str = Field(..., description="Resolved file path that was edited.")
    status: str = Field(..., description="Edit result: edited, aborted_version, or failed.")
    changed_paths: list[str] = Field(default_factory=list, description="Files changed by the edits.")
    changed_path_kinds: dict[str, str] = Field(
        default_factory=dict,
        description="Changed paths keyed to write/delete/symlink/opaque_dir.",
    )
    mutation_source: str = Field(default="", description="Mutation source tag.")
    conflict_reason: str | None = Field(
        default=None, description="Conflict reason when the edits failed."
    )
    error: dict[str, object] = Field(default_factory=dict, description="Typed error payload.")
    applied_edits: int = Field(
        default=0,
        description="Number of edits applied (not occurrence count).",
    )


@tool(
    name="multi_edit",
    description=get_multi_edit_description(),
    short_description="Apply an ordered batch of edits to one file atomically.",
    input_model=MultiEditInput,
    output_model=MultiEditOutput,
    intent=Intent.WRITE_ALLOWED,
)
async def multi_edit(
    file_path: str,
    edits: list[dict[str, object]],
    description: str = "",
    *,
    context: ToolExecutionContextService,
) -> ToolResult:
    """Apply an ordered batch of edits to one file."""
    file_path = resolve_tool_sandbox_path(file_path, context)

    if not edits:
        return ToolResult(
            output="Provide at least one edit in `edits`.",
            is_error=True,
        )

    sandbox_id, sandbox_id_error = sandbox_id_or_missing_error_result(context)
    if sandbox_id_error is not None:
        return sandbox_id_error

    search_replace_edits = tuple(
        SearchReplaceEdit(
            old_text=str(op.get("old_text", "")),
            new_text=str(op.get("new_text", "")),
            replace_all=bool(op.get("replace_all", False)),
        )
        for op in edits
    )

    result = await sandbox_api.edit_file(
        sandbox_id,
        EditFileRequest(
            path=file_path,
            edits=search_replace_edits,
            caller=sandbox_caller_from_tool_context(context),
            description=description or f"multi-edit {file_path}",
        ),
        **sandbox_audit_kwargs_from_tool_context(context),
    )

    return project_file_mutation(
        result,
        success_status="edited",
        file_path=file_path,
        success_extra={"applied_edits": len(search_replace_edits)},
        context=context,
    )


__all__ = ["multi_edit"]
