"""Delete file tool."""

from __future__ import annotations

from pydantic import BaseModel, Field

from code_intelligence.types import DeleteSpec
from tools.core.base import ToolExecutionContextService, ToolResult
from tools.core.ci_runtime import ci_write_required_result, get_ci_service
from tools.core.decorator import tool
from tools.daytona_toolkit._commit import submit_commit
from tools.daytona_toolkit._daytona_utils import _resolve_path
from tools.daytona_toolkit._delete_move_helpers import (
    failure_status,
    normalized_path,
    operation_payload,
)


class DeleteFileInput(BaseModel):
    path: str = Field(
        ...,
        min_length=1,
        description="Repo-relative or sandbox-root file or folder path.",
    )
    is_folder: bool = Field(
        default=False,
        description="False deletes one file. True deletes a folder tree.",
    )


class DeleteFileOutput(BaseModel):
    status: str = Field(
        ...,
        description="`deleted`, `not_found`, `aborted_version`, `aborted_lock`, or `failed`.",
    )
    paths: list[str] = Field(
        default_factory=list,
        description="Paths changed by the delete.",
    )
    warnings: list[str] = Field(
        default_factory=list,
        description="Advisory warnings emitted by the operation.",
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
    name="delete_file",
    description="Delete a sandbox file or folder.",
    short_description="Delete a file or folder.",
    input_model=DeleteFileInput,
    output_model=DeleteFileOutput,
)
async def delete_file(
    path: str,
    is_folder: bool = False,
    *,
    context: ToolExecutionContextService,
) -> ToolResult:
    """Delete a file or folder."""
    resolved = normalized_path(_resolve_path(path, context))
    warnings: list[str] = []

    svc = get_ci_service(context)
    if svc is None:
        return ci_write_required_result("delete_file", resolved)

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
            output=operation_payload(
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

    payload_status, conflict_reason = failure_status(change.raw, move=False)
    return ToolResult(
        output=operation_payload(
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


__all__ = ["delete_file"]
