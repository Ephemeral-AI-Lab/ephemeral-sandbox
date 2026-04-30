"""Delete file tool."""

from __future__ import annotations

from pydantic import BaseModel, Field

from sandbox.code_intelligence.core.types import DeleteSpec
from tools.core.base import ToolExecutionContextService, ToolResult
from tools.core.decorator import tool
from sandbox.commit import commit_metadata, failure_status, submit_commit
from tools.daytona_toolkit._mutation_helpers import ci_write_guard
from sandbox.daytona_utils import _normalized_path, _resolve_path
from tools.daytona_toolkit._delete_move_helpers import operation_payload


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
    description=(
        "Delete a file, or a folder tree with `is_folder=True`. Atomic and audited via the "
        "commit pipeline. Prefer over `shell rm` for structured errors and traceability. Don't "
        "use to \"clear\" a file you intend to rewrite — just `write_file` over it. Returns "
        "`not_found`, `aborted_version`, or `aborted_lock` on common non-success paths."
    ),
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
    resolved = _normalized_path(_resolve_path(path, context))
    warnings: list[str] = []

    if guard := ci_write_guard(context, tool_name="delete_file", path=resolved):
        return guard

    specs = [DeleteSpec(path=resolved, is_folder=is_folder)]

    change = await submit_commit(
        context,
        op="delete",
        specs=specs,
        fallback_paths=[resolved],
        description=f"delete {resolved}",
    )
    paths = list(change.changed_paths)
    common_metadata = commit_metadata(change, paths)
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
