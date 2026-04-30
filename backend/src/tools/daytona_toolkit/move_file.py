"""Move file tool."""

from __future__ import annotations

from pydantic import BaseModel, Field

from sandbox.code_intelligence.core.types import MoveSpec
from tools.core.base import ToolExecutionContextService, ToolResult
from tools.core.decorator import tool
from sandbox.commit import commit_metadata, failure_status, submit_commit
from tools.daytona_toolkit._mutation_helpers import ci_write_guard
from sandbox.daytona_utils import _normalized_path, _resolve_path
from tools.daytona_toolkit._delete_move_helpers import move_payload


class MoveFileInput(BaseModel):
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
        description="False moves one file. True moves a folder tree.",
    )


class MoveFileOutput(BaseModel):
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
    name="move_file",
    description=(
        "Move or rename a file, or a folder tree with `is_folder=True`. Atomic via the commit "
        "pipeline. Prefer over `shell mv`. Refuses to overwrite an existing destination "
        "(`status: dst_exists`) — delete it first if intended. There is no copy tool; for a "
        "copy, `read_file` then `write_file`. Parent of target must exist."
    ),
    short_description="Move a file or folder.",
    input_model=MoveFileInput,
    output_model=MoveFileOutput,
)
async def move_file(
    src_path: str,
    target_path: str,
    is_folder: bool = False,
    *,
    context: ToolExecutionContextService,
) -> ToolResult:
    """Move a file or folder."""
    src_resolved = _normalized_path(_resolve_path(src_path, context))
    dst_resolved = _normalized_path(_resolve_path(target_path, context))
    warnings: list[str] = []

    if guard := ci_write_guard(context, tool_name="move_file", path=src_resolved):
        return guard

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
    common_metadata = commit_metadata(change, paths)

    if change.success:
        return ToolResult(
            output=move_payload(
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

    payload_status, conflict_reason = failure_status(change.raw, move=True)
    return ToolResult(
        output=move_payload(
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


__all__ = ["move_file"]
