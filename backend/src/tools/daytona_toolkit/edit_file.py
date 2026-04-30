"""Edit file tool."""

from __future__ import annotations

import logging
import time
from typing import Any

from pydantic import BaseModel, ConfigDict, Field

from sandbox.code_intelligence.mutations.patcher import SearchReplaceEdit
from sandbox.code_intelligence.core.types import EditSpec
from tools.core.base import ToolExecutionContextService, ToolResult
from tools.core.decorator import tool
from tools.core.op_result_to_tool_result import operation_result_to_tool_result
from sandbox.commit import commit_metadata, submit_commit
from tools.daytona_toolkit._mutation_helpers import ci_write_guard
from sandbox.daytona_utils import (
    _get_repo_root,
    _resolve_path,
)

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
    warnings: list[str] = Field(default_factory=list, description="Non-fatal edit warnings.")
    timings: dict[str, Any] | None = Field(
        default=None,
        description="Optional edit timing metadata.",
    )
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
    tool_started = time.perf_counter()
    tool_timings: dict[str, float] = {}

    file_path = _resolve_path(file_path, context)
    warnings: list[str] = []

    normalized_edits, edit_error = _normalize_edits(
        old_text=old_text,
        new_text=new_text,
    )
    if edit_error is not None:
        body = (
            f"{edit_error}\n\n" + "\n".join(warnings) if warnings else edit_error
        )
        return ToolResult(output=body, is_error=True)

    if guard := ci_write_guard(context, tool_name="edit_file", path=file_path):
        return guard

    commit_started = time.perf_counter()
    change = await submit_commit(
        context,
        op="edit",
        specs=[EditSpec(file_path=file_path, edits=normalized_edits)],
        fallback_paths=[file_path],
        description=description or f"edit {file_path}",
    )
    tool_timings["commit"] = round(time.perf_counter() - commit_started, 6)

    metadata_extra = commit_metadata(change)

    if not change.success:
        return _edit_failure_result(
            change.raw,
            file_path=file_path,
            warnings=warnings,
            metadata_extra=metadata_extra,
        )

    tool_timings["tool_total"] = round(time.perf_counter() - tool_started, 6)
    return operation_result_to_tool_result(
        change.raw,
        tool_name="edit_file",
        success_status="edited",
        primary_paths=[file_path],
        warnings=warnings,
        success_extra={
            "cwd": _get_repo_root(context) or "",
            "file_path": file_path,
            "applied_edits": len(normalized_edits),
            "timings": {"tool": tool_timings},
        },
        metadata_extra=metadata_extra,
    )


def _edit_failure_result(
    result: Any,
    *,
    file_path: str,
    warnings: list[str],
    metadata_extra: dict[str, Any] | None = None,
) -> ToolResult:
    """Return the user-facing error for a failed edit."""
    return operation_result_to_tool_result(
        result,
        tool_name="edit_file",
        success_status="edited",
        primary_paths=[file_path],
        warnings=warnings,
        metadata_extra=metadata_extra,
    )
