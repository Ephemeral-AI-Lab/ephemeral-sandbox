"""Daytona delete and move tools."""

from __future__ import annotations

import json
from typing import Any

from pydantic import BaseModel, Field

from code_intelligence.types import DeleteSpec, MoveSpec
from tools.core.base import ToolExecutionContext, ToolResult
from tools.core.ci_runtime import (
    ci_write_required_result,
    get_ci_service,
)
from tools.core.decorator import tool
from tools.daytona_toolkit._commit import submit_commit
from tools.daytona_toolkit._daytona_utils import _resolve_path


# ---------------------------------------------------------------------------
# Shared helpers
# ---------------------------------------------------------------------------


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


def _move_payload(
    *,
    status: str,
    src: str,
    dst: str,
    warnings: list[str],
    paths: list[str] | None = None,
    conflict_reason: str | None = None,
    message: str | None = None,
) -> str:
    payload: dict[str, Any] = {
        "status": status,
        "src_path": src,
        "target_path": dst,
        "paths": (
            paths
            if paths is not None
            else ([src, dst] if status == "moved" else [])
        ),
        "warnings": warnings,
    }
    if conflict_reason:
        payload["conflict_reason"] = conflict_reason
    if message:
        payload["message"] = message
    return json.dumps(payload)


def _normalized_path(path: str) -> str:
    if path == "/":
        return path
    return path.rstrip("/") or path


def _failure_status(result: Any, *, move: bool) -> tuple[str, str]:
    status = str(getattr(result, "status", "") or "failed")
    conflict_reason = str(getattr(result, "conflict_reason", "") or "")
    if conflict_reason == "not_found":
        return "not_found", "not_found"
    if move and conflict_reason == "dst_exists":
        return "dst_exists", "dst_exists"
    return status, conflict_reason or status


# ---------------------------------------------------------------------------
# daytona_delete_file
# ---------------------------------------------------------------------------


class DaytonaDeleteFileInput(BaseModel):
    path: str = Field(
        ...,
        min_length=1,
        description="Repo-relative or sandbox-root file or folder path.",
    )
    is_folder: bool = Field(
        default=False,
        description=(
            "False deletes one file. True deletes a folder tree."
        ),
    )


class DaytonaDeleteFileOutput(BaseModel):
    status: str = Field(
        ...,
        description=(
            "`deleted`, `not_found`, `aborted_version`, `aborted_lock`, or `failed`."
        ),
    )
    paths: list[str] = Field(
        default_factory=list,
        description="Paths changed by the delete.",
    )
    warnings: list[str] = Field(
        default_factory=list,
        description="Advisory warnings emitted by platform hooks.",
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
    description="Delete a sandbox file or folder.",
    short_description="Delete a file or folder.",
    input_model=DaytonaDeleteFileInput,
    output_model=DaytonaDeleteFileOutput,
)
async def daytona_delete_file(
    path: str,
    is_folder: bool = False,
    *,
    context: ToolExecutionContext,
) -> ToolResult:
    """Delete a file or folder."""
    resolved = _normalized_path(_resolve_path(path, context))
    warnings: list[str] = []

    svc = get_ci_service(context)
    if svc is None:
        return ci_write_required_result("daytona_delete_file", resolved)

    specs = [DeleteSpec(path=resolved, is_folder=is_folder)]

    change = await submit_commit(
        context,
        op="delete",
        specs=specs,
        fallback_paths=[resolved],
        description=f"delete {resolved}",
    )
    paths = list(change.changed_paths)
    common_metadata = {
        "changed_paths": paths,
        "ambient_changed_paths": list(change.ambient_changed_paths),
        "conflict_reason": change.conflict_reason,
    }
    if change.success:
        return ToolResult(
            output=_operation_payload(
                status="deleted",
                paths=paths,
                warnings=warnings,
            ),
            metadata={
                "file_count": len(paths),
                "success_count": len(paths),
                **common_metadata,
            },
        )

    payload_status, conflict_reason = _failure_status(change.raw, move=False)
    return ToolResult(
        output=_operation_payload(
            status=payload_status,
            paths=paths,
            warnings=warnings,
            conflict_reason=conflict_reason,
            message=str(change.conflict_reason or conflict_reason),
        ),
        is_error=True,
        metadata={
            "file_count": len(paths),
            "success_count": 0,
            **common_metadata,
        },
    )


# ---------------------------------------------------------------------------
# daytona_move_file
# ---------------------------------------------------------------------------


class DaytonaMoveFileInput(BaseModel):
    src_path: str = Field(
        ...,
        min_length=1,
        description="Repo-relative or sandbox-root source path.",
    )
    target_path: str = Field(
        ...,
        min_length=1,
        description="Repo-relative or sandbox-root destination path.",
    )
    is_folder: bool = Field(
        default=False,
        description=(
            "False moves one file. True moves a folder tree."
        ),
    )


class DaytonaMoveFileOutput(BaseModel):
    status: str = Field(
        ...,
        description=(
            "`moved`, `dst_exists`, `not_found`, `aborted_version`, "
            "`aborted_overlap`, `aborted_lock`, or `failed`."
        ),
    )
    src_path: str = Field(..., description="Resolved source path.")
    target_path: str = Field(..., description="Resolved destination path.")
    paths: list[str] = Field(
        default_factory=list,
        description="Paths changed by the move.",
    )
    warnings: list[str] = Field(
        default_factory=list,
        description="Advisory warnings emitted by platform hooks.",
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
    description="Move a sandbox file or folder.",
    short_description="Move a file or folder.",
    input_model=DaytonaMoveFileInput,
    output_model=DaytonaMoveFileOutput,
)
async def daytona_move_file(
    src_path: str,
    target_path: str,
    is_folder: bool = False,
    *,
    context: ToolExecutionContext,
) -> ToolResult:
    """Move a file or folder."""
    src_resolved = _normalized_path(_resolve_path(src_path, context))
    dst_resolved = _normalized_path(_resolve_path(target_path, context))
    warnings: list[str] = []

    svc = get_ci_service(context)
    if svc is None:
        return ci_write_required_result("daytona_move_file", src_resolved)

    specs = [
        MoveSpec(
            src_path=src_resolved,
            dst_path=dst_resolved,
            overwrite=False,
            is_folder=is_folder,
        ),
    ]
    fallback_paths = [s.src_path for s in specs] + [s.dst_path for s in specs]

    change = await submit_commit(
        context,
        op="move",
        specs=specs,
        fallback_paths=fallback_paths,
        description=f"move {src_resolved} -> {dst_resolved}",
    )
    paths = list(change.changed_paths)
    common_metadata = {
        "changed_paths": paths,
        "ambient_changed_paths": list(change.ambient_changed_paths),
        "conflict_reason": change.conflict_reason,
    }

    if change.success:
        return ToolResult(
            output=_move_payload(
                status="moved",
                src=src_resolved,
                dst=dst_resolved,
                paths=paths,
                warnings=warnings,
            ),
            metadata={
                "file_count": len(paths),
                "success_count": len(paths),
                **common_metadata,
            },
        )

    payload_status, conflict_reason = _failure_status(change.raw, move=True)
    return ToolResult(
        output=_move_payload(
            status=payload_status,
            src=src_resolved,
            dst=dst_resolved,
            paths=paths,
            warnings=warnings,
            conflict_reason=conflict_reason,
            message=str(change.conflict_reason or conflict_reason),
        ),
        is_error=True,
        metadata={
            "file_count": len(paths),
            "success_count": 0,
            **common_metadata,
        },
    )
