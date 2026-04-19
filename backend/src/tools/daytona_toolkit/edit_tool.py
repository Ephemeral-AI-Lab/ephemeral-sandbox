"""File editing tool for Daytona sandboxes.

Atomic search/replace edits flow through the code-intelligence OCC commit
path (``svc.edit_file``) so every tool call = one OCC batch = atomic
across its edits, with drift detection handled by the coordinator's
strict-base branch.
"""

from __future__ import annotations

import logging
import time
from typing import Any

from pydantic import BaseModel, Field

from code_intelligence.editing.patcher import SearchReplaceEdit
from code_intelligence.types import EditSpec
from tools.core.base import ToolExecutionContext, ToolResult
from tools.core.ci_runtime import ci_write_required_result, get_ci_service
from tools.core.decorator import tool
from tools.core.op_result_to_tool_result import operation_result_to_tool_result
from tools.daytona_toolkit._commit import submit_commit
from tools.daytona_toolkit.tools import (
    _get_cwd,
    _resolve_path,
)

logger = logging.getLogger(__name__)


class DaytonaEditFileInput(BaseModel):
    file_path: str = Field(..., description="Path to the file to edit.")
    old_text: str = Field(
        default="",
        description="Exact text to find in single-edit mode. Pair only with new_text.",
    )
    new_text: str = Field(
        default="",
        description="Replacement text for single-edit mode. Do not send with edits.",
    )
    edits: list[dict[str, Any]] | None = Field(
        default=None,
        description=(
            "Optional batch of edit objects. Supported shape: "
            "{\"strategy\":\"search_replace\",\"search\":\"...\",\"replace\":\"...\"}."
        ),
    )
    description: str = Field(
        default="",
        description="Optional human-readable description of the edit.",
    )


class DaytonaEditFileOutput(BaseModel):
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
    edits: list[dict[str, Any]] | None,
) -> tuple[list[SearchReplaceEdit], str | None, bool]:
    """Validate and normalize tool inputs into patcher edit objects.

    Returns ``(normalized_edits, error_message, legacy_single_edit)``.
    ``legacy_single_edit`` is ``True`` when the caller used the
    top-level ``old_text``/``new_text`` pair rather than the ``edits``
    list, so the error path can mimic the historical message shape.
    """
    if edits is not None:
        if old_text or new_text:
            return [], "Provide either `old_text`/`new_text` or `edits`, not both.", False
        normalized: list[SearchReplaceEdit] = []
        for index, edit in enumerate(edits, start=1):
            if not isinstance(edit, dict):
                return [], f"Edit {index}: each edit must be an object.", False
            strategy = str(edit.get("strategy") or "").strip()
            if not strategy:
                if {"old_text", "new_text", "old_string", "new_string", "search", "replace"} & set(edit):
                    strategy = "search_replace"
            if strategy != "search_replace":
                return [], (
                    f"Edit {index}: unknown strategy '{strategy}'. "
                    "Use `{\"strategy\": \"search_replace\", \"search\": \"...\", \"replace\": \"...\"}` "
                    "or top-level `old_text`/`new_text` for a single edit."
                ), False
            search = edit.get("search") or edit.get("old_text") or edit.get("old_string")
            replace = edit.get("replace") or edit.get("new_text") or edit.get("new_string")
            if not isinstance(search, str) or not isinstance(replace, str):
                return (
                    [],
                    f"Edit {index}: search_replace requires string `search` and `replace`.",
                    False,
                )
            normalized.append(SearchReplaceEdit(old_text=search, new_text=replace))
        if not normalized:
            return [], "At least one edit is required.", False
        return normalized, None, False

    if not old_text:
        return [], (
            "Provide `old_text` (text to find) and `new_text` (replacement), "
            "or use `edits` with strategy `search_replace`."
        ), False
    return [SearchReplaceEdit(old_text=old_text, new_text=new_text)], None, True


@tool(
    name="daytona_edit_file",
    description=(
        "Edit a file atomically through the OCC-gated code-intelligence "
        "commit path. Use exactly one mode: "
        "(1) `old_text` + `new_text` for a single replacement or "
        "(2) `edits=[{\"strategy\":\"search_replace\",\"search\":\"...\",\"replace\":\"...\"}]` "
        "for batched replacements. Never send `new_text` together with `edits`. "
        "Before calling, compare `file_path` to your `scope_paths`; if it is outside "
        "scope, make an explicit widened-edit decision. "
        "In coordinated team lanes, outside-scope writes are advisory, not a hard gate. "
        "Proceed only when the target is a justified adjacent production owner for the same bug, "
        "including a missing module, compatibility shim, re-export, or import bridge when production ownership is clear; "
        "otherwise submit `submit_task_summary(type='request_replan')` so replanning can widen "
        "or resequence the task. Test imports, collection errors, and target counts naming "
        "the path are evidence, not sufficient ownership by themselves. In coordinated team lanes, test files are "
        "read/verify-only and this tool blocks test-file writes unless explicit "
        "authorization is present. "
        "If you continue after an outside-scope warning, include the widened path, rationale, and verification in the terminal summary."
    ),
    short_description="Apply atomic file edits.",
    input_model=DaytonaEditFileInput,
    output_model=DaytonaEditFileOutput,
)
async def daytona_edit_file(
    file_path: str,
    old_text: str = "",
    new_text: str = "",
    edits: list[dict[str, Any]] | None = None,
    description: str = "",
    *,
    context: ToolExecutionContext,
) -> ToolResult:
    """Edit a file atomically through ``svc.edit_file``."""
    tool_started = time.perf_counter()
    tool_timings: dict[str, float] = {}

    file_path = _resolve_path(file_path, context)
    warnings: list[str] = []

    normalized_edits, edit_error, legacy_single_edit = _normalize_edits(
        old_text=old_text,
        new_text=new_text,
        edits=edits,
    )
    if edit_error is not None:
        body = (
            f"{edit_error}\n\n" + "\n".join(warnings) if warnings else edit_error
        )
        return ToolResult(output=body, is_error=True)

    if get_ci_service(context) is None:
        return ci_write_required_result("daytona_edit_file", file_path)

    commit_started = time.perf_counter()
    change = await submit_commit(
        context,
        op="edit",
        specs=[EditSpec(file_path=file_path, edits=normalized_edits)],
        fallback_paths=[file_path],
        description=description or f"edit {file_path}",
    )
    tool_timings["commit"] = round(time.perf_counter() - commit_started, 6)

    metadata_extra = {
        "changed_paths": list(change.changed_paths),
        "ambient_changed_paths": list(change.ambient_changed_paths),
        "conflict_reason": change.conflict_reason,
    }

    if not change.success:
        return _edit_failure_result(
            change.raw,
            file_path=file_path,
            warnings=warnings,
            legacy_single_edit=legacy_single_edit,
            metadata_extra=metadata_extra,
        )

    tool_timings["tool_total"] = round(time.perf_counter() - tool_started, 6)
    return operation_result_to_tool_result(
        change.raw,
        tool_name="daytona_edit_file",
        success_status="edited",
        primary_paths=[file_path],
        warnings=warnings,
        success_extra={
            "cwd": _get_cwd(context) or "",
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
    legacy_single_edit: bool,
    metadata_extra: dict[str, Any] | None = None,
) -> ToolResult:
    """Translate a failed :class:`OperationResult` into a tool-facing error.

    ``legacy_single_edit`` preserves the pre-migration error text for the
    common "search text not found" case so callers that match on the
    message keep working.
    """
    if (
        legacy_single_edit
        and result.conflict_reason == "patch_failed"
    ):
        return ToolResult(
            output=f"Search text not found in {file_path}",
            is_error=True,
            metadata=dict(metadata_extra or {}),
        )
    return operation_result_to_tool_result(
        result,
        tool_name="daytona_edit_file",
        success_status="edited",
        primary_paths=[file_path],
        warnings=warnings,
        metadata_extra=metadata_extra,
    )
