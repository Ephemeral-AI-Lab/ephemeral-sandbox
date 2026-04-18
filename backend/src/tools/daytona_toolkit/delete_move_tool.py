"""Daytona-backed file delete and move tools.

These tools route every mutation through
:meth:`CodeIntelligenceService.delete_file` and
:meth:`CodeIntelligenceService.move_file`, which build
:class:`OperationChange` tuples with real ``base_hash`` values and commit them
via :meth:`WriteCoordinator.commit_operation_against_base`. The coordinator
enforces the OCC guard (``current_hash == base_hash``), sorted-path locks,
TimeMachine snapshots, and symbol-index refresh for every touched path. No
filesystem command is issued from the tool layer — there is no way for these
tools to bypass the base-hash check.

CodeAct's shell policy blocks ``rm`` / ``mv`` precisely so that deletions and
moves flow through these audited tools instead of the unaudited shell path.
"""

from __future__ import annotations

import asyncio
import json
import logging
from typing import Any

from pydantic import BaseModel, Field

from tools.core.base import ToolExecutionContext, ToolResult
from tools.core.ci_runtime import (
    ci_write_required_result,
    get_ci_service,
)
from tools.core.decorator import tool
from tools.daytona_toolkit._daytona_utils import (
    _require_sandbox,
    _resolve_path,
    _team_repo_write_error,
    _team_repo_write_warning,
    record_coordination_warning,
)

logger = logging.getLogger(__name__)


# ---------------------------------------------------------------------------
# Shared helpers
# ---------------------------------------------------------------------------


def _scope_checks(
    context: ToolExecutionContext,
    file_path: str,
    *,
    tool_name: str,
) -> tuple[str | None, str | None]:
    """Apply write-scope policy; return ``(hard_error, soft_warning)``."""
    err = _team_repo_write_error(context, file_path, tool_name=tool_name)
    if err is not None:
        return err, None
    warn = _team_repo_write_warning(context, file_path, tool_name=tool_name)
    return None, warn


def _operation_payload(
    *,
    status: str,
    paths: list[str],
    warnings: list[str],
    conflict_reason: str | None = None,
    message: str | None = None,
) -> str:
    payload: dict[str, Any] = {
        "status": status,
        "paths": paths,
        "warnings": warnings,
    }
    if conflict_reason:
        payload["conflict_reason"] = conflict_reason
    if message:
        payload["message"] = message
    return json.dumps(payload)


# ---------------------------------------------------------------------------
# daytona_delete_file
# ---------------------------------------------------------------------------


class DaytonaDeleteFileInput(BaseModel):
    file_path: str = Field(
        ...,
        min_length=1,
        description="Path to the file to delete. Must exist at call time.",
    )
    description: str = Field(
        default="",
        description="Optional human-readable description of the delete.",
    )


class DaytonaDeleteFileOutput(BaseModel):
    status: str = Field(
        ...,
        description=(
            "`deleted`, `aborted_version` (base-hash mismatch), "
            "`aborted_lock`, `not_found`, or `failed`."
        ),
    )
    paths: list[str] = Field(
        default_factory=list,
        description="Paths affected (one entry for a successful delete).",
    )
    warnings: list[str] = Field(
        default_factory=list,
        description="Advisory warnings (e.g., files outside write_scope).",
    )
    conflict_reason: str | None = Field(
        default=None,
        description="Short reason when status is an abort class.",
    )
    message: str | None = Field(
        default=None,
        description="Human-readable detail.",
    )


@tool(
    name="daytona_delete_file",
    description=(
        "Delete a single file atomically through the OCC-gated commit path. "
        "The service reads the current content, captures its base_hash, and "
        "submits an OperationChange(final_content=None) to the write "
        "coordinator. Any drift between the read and the commit aborts with "
        "status=aborted_version and leaves the file in place — there is no "
        "merge fallback for deletes. Use this instead of attempting `rm` in "
        "CodeAct; the shell policy blocks `rm` for that reason."
    ),
    short_description="Delete a file atomically with OCC.",
    input_model=DaytonaDeleteFileInput,
    output_model=DaytonaDeleteFileOutput,
)
async def daytona_delete_file(
    file_path: str,
    description: str = "",
    *,
    context: ToolExecutionContext,
) -> ToolResult:
    """Delete a file in the Daytona sandbox with OCC."""
    file_path = _resolve_path(file_path, context)
    hard_error, soft_warning = _scope_checks(
        context, file_path, tool_name="daytona_delete_file",
    )
    if hard_error is not None:
        return ToolResult(output=hard_error, is_error=True)

    warnings: list[str] = []
    if soft_warning is not None:
        warnings.append(soft_warning)
        record_coordination_warning(
            context, category="write_scope", message=soft_warning,
        )

    svc = get_ci_service(context)
    if svc is None:
        return ci_write_required_result("daytona_delete_file", file_path)

    try:
        sandbox = await _require_sandbox(context)
    except Exception as exc:
        return ToolResult(output=str(exc), is_error=True)
    svc.rebind_sandbox(sandbox)

    agent_id = str(context.metadata.get("agent_name") or context.metadata.get("agent_run_id") or "")
    try:
        result = await asyncio.to_thread(
            svc.delete_file,
            file_path,
            agent_id=agent_id,
            description=description or f"delete {file_path}",
        )
    except Exception as exc:
        logger.debug("delete_file raised for %s", file_path, exc_info=True)
        return ToolResult(
            output=_operation_payload(
                status="failed",
                paths=[file_path],
                warnings=warnings,
                message=f"Delete failed: {exc}",
            ),
            is_error=True,
        )

    if result.success:
        return ToolResult(
            output=_operation_payload(
                status="deleted",
                paths=[file_path],
                warnings=warnings,
            ),
            metadata={"file_count": 1, "success_count": 1},
        )

    # Translate known coordinator statuses to tool-visible strings.
    status = result.status
    conflict_reason = result.conflict_reason or status
    if conflict_reason == "not_found":
        payload_status = "not_found"
        is_error = True
    elif status.startswith("aborted"):
        payload_status = status
        is_error = True
    else:
        payload_status = "failed"
        is_error = True

    message = (
        result.files[0].message
        if result.files
        else conflict_reason
    )
    return ToolResult(
        output=_operation_payload(
            status=payload_status,
            paths=[file_path],
            warnings=warnings,
            conflict_reason=conflict_reason,
            message=message,
        ),
        is_error=is_error,
        metadata={"file_count": 1, "success_count": 0},
    )


# ---------------------------------------------------------------------------
# daytona_move_file
# ---------------------------------------------------------------------------


class DaytonaMoveFileInput(BaseModel):
    src_path: str = Field(
        ...,
        min_length=1,
        description="Source file path. Must exist at call time.",
    )
    dst_path: str = Field(
        ...,
        min_length=1,
        description="Destination file path. Must not exist unless overwrite=True.",
    )
    overwrite: bool = Field(
        default=False,
        description=(
            "When True, replace an existing destination. The destination "
            "change is committed with strict_base so any concurrent edit "
            "on the destination aborts with aborted_version rather than "
            "being silently merged."
        ),
    )
    description: str = Field(
        default="",
        description="Optional human-readable description of the move.",
    )


class DaytonaMoveFileOutput(BaseModel):
    status: str = Field(
        ...,
        description=(
            "`moved`, `aborted_version`, `aborted_lock`, `dst_exists`, "
            "`not_found`, or `failed`."
        ),
    )
    src_path: str = Field(..., description="Resolved source path.")
    dst_path: str = Field(..., description="Resolved destination path.")
    paths: list[str] = Field(
        default_factory=list,
        description="Paths affected ([src, dst] for a successful move).",
    )
    warnings: list[str] = Field(
        default_factory=list,
        description="Advisory warnings (e.g., files outside write_scope).",
    )
    conflict_reason: str | None = Field(
        default=None,
        description="Short reason when status is an abort class.",
    )
    message: str | None = Field(
        default=None,
        description="Human-readable detail.",
    )


@tool(
    name="daytona_move_file",
    description=(
        "Atomically move a file (delete source + write destination) through "
        "the OCC-gated commit path. Both slots are submitted as one "
        "OperationChange batch so sorted-path locks, base-hash checks, and "
        "TimeMachine rollback make the move atomic: if either slot fails "
        "its OCC guard, neither touches disk. By default the destination "
        "must not exist; pass overwrite=True to replace it (the destination "
        "change is strict_base, so any concurrent edit aborts rather than "
        "being silently merged). Use this instead of attempting `mv` in "
        "CodeAct; the shell policy blocks `mv` for that reason."
    ),
    short_description="Move a file atomically with OCC.",
    input_model=DaytonaMoveFileInput,
    output_model=DaytonaMoveFileOutput,
)
async def daytona_move_file(
    src_path: str,
    dst_path: str,
    overwrite: bool = False,
    description: str = "",
    *,
    context: ToolExecutionContext,
) -> ToolResult:
    """Move a file in the Daytona sandbox with OCC."""
    src_resolved = _resolve_path(src_path, context)
    dst_resolved = _resolve_path(dst_path, context)

    warnings: list[str] = []
    for path in (src_resolved, dst_resolved):
        hard_error, soft_warning = _scope_checks(
            context, path, tool_name="daytona_move_file",
        )
        if hard_error is not None:
            return ToolResult(output=hard_error, is_error=True)
        if soft_warning is not None:
            warnings.append(soft_warning)
            record_coordination_warning(
                context, category="write_scope", message=soft_warning,
            )

    svc = get_ci_service(context)
    if svc is None:
        return ci_write_required_result("daytona_move_file", src_resolved)

    try:
        sandbox = await _require_sandbox(context)
    except Exception as exc:
        return ToolResult(output=str(exc), is_error=True)
    svc.rebind_sandbox(sandbox)

    agent_id = str(context.metadata.get("agent_name") or context.metadata.get("agent_run_id") or "")
    try:
        result = await asyncio.to_thread(
            svc.move_file,
            src_resolved,
            dst_resolved,
            overwrite=overwrite,
            agent_id=agent_id,
            description=description or f"move {src_resolved} -> {dst_resolved}",
        )
    except Exception as exc:
        logger.debug(
            "move_file raised for %s -> %s", src_resolved, dst_resolved, exc_info=True,
        )
        return ToolResult(
            output=_move_payload(
                status="failed",
                src=src_resolved,
                dst=dst_resolved,
                warnings=warnings,
                message=f"Move failed: {exc}",
            ),
            is_error=True,
        )

    if result.success:
        return ToolResult(
            output=_move_payload(
                status="moved",
                src=src_resolved,
                dst=dst_resolved,
                warnings=warnings,
            ),
            metadata={"file_count": 2, "success_count": 2},
        )

    status = result.status
    conflict_reason = result.conflict_reason or status
    if conflict_reason == "dst_exists":
        payload_status = "dst_exists"
    elif conflict_reason == "not_found":
        payload_status = "not_found"
    elif conflict_reason == "identical_paths":
        payload_status = "failed"
    elif status.startswith("aborted"):
        payload_status = status
    else:
        payload_status = "failed"

    message = (
        result.files[0].message
        if result.files
        else conflict_reason
    )
    return ToolResult(
        output=_move_payload(
            status=payload_status,
            src=src_resolved,
            dst=dst_resolved,
            warnings=warnings,
            conflict_reason=conflict_reason,
            message=message,
        ),
        is_error=True,
        metadata={"file_count": 2, "success_count": 0},
    )


def _move_payload(
    *,
    status: str,
    src: str,
    dst: str,
    warnings: list[str],
    conflict_reason: str | None = None,
    message: str | None = None,
) -> str:
    payload: dict[str, Any] = {
        "status": status,
        "src_path": src,
        "dst_path": dst,
        "paths": [src, dst] if status == "moved" else [],
        "warnings": warnings,
    }
    if conflict_reason:
        payload["conflict_reason"] = conflict_reason
    if message:
        payload["message"] = message
    return json.dumps(payload)
