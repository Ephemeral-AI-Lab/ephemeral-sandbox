"""Glob tool: enumerate workspace paths matching a pattern."""

from __future__ import annotations

import json

from pydantic import BaseModel, ConfigDict, Field

import sandbox.api as sandbox_api
from sandbox._shared.models import Intent
from sandbox.api import GlobRequest
from tools._framework.core.base import ToolExecutionContextService, ToolResult
from tools._framework.core.decorator import tool
from .prompt import get_glob_description
from tools.sandbox._lib.tool_context import (
    sandbox_audit_kwargs_from_tool_context,
    sandbox_caller_from_tool_context,
    sandbox_repo_root_from_tool_context,
    sandbox_path_error_message,
    resolve_tool_sandbox_path,
    sandbox_audit_metadata_from_tool_context,
    sandbox_id_or_missing_error_result,
)


class GlobInput(BaseModel):
    model_config = ConfigDict(extra="forbid")

    pattern: str = Field(
        ...,
        description=(
            "fnmatch-style glob pattern applied against workspace-relative paths "
            "(e.g. '*.py' matches every Python file; 'pkg/*.py' restricts to a "
            "directory)."
        ),
    )
    path: str | None = Field(
        default=None,
        description=(
            "Optional workspace-relative or sandbox-root directory to restrict "
            "the search to. Defaults to the entire workspace snapshot."
        ),
    )


class GlobOutput(BaseModel):
    cwd: str = Field(..., description="Current sandbox working directory.")
    pattern: str = Field(..., description="Glob pattern that was applied.")
    filenames: list[str] = Field(
        default_factory=list,
        description="Workspace-relative paths that matched the pattern.",
    )
    num_files: int = Field(
        default=0,
        description="Number of matched paths returned (post-cap).",
    )
    truncated: bool = Field(
        default=False,
        description="True when the result set was capped at 100 paths.",
    )


@tool(
    name="glob",
    description=get_glob_description(),
    short_description="Find workspace files by glob pattern.",
    input_model=GlobInput,
    output_model=GlobOutput,
    intent=Intent.READ_ONLY,
)
async def glob(
    pattern: str,
    path: str | None = None,
    *,
    context: ToolExecutionContextService,
) -> ToolResult:
    """Glob workspace paths."""
    resolved_path = resolve_tool_sandbox_path(path, context) if path else None
    sandbox_id, sandbox_id_error = sandbox_id_or_missing_error_result(context)
    if sandbox_id_error is not None:
        return sandbox_id_error
    try:
        result = await sandbox_api.glob(
            sandbox_id,
            GlobRequest(
                pattern=pattern,
                path=resolved_path,
                caller=sandbox_caller_from_tool_context(context),
            ),
            **sandbox_audit_kwargs_from_tool_context(context),
        )
        if not result.success:
            raise RuntimeError(f"glob failed for pattern: {pattern}")
        metadata: dict[str, object] = {}
        if result.timings:
            metadata["timings"] = dict(result.timings)
        metadata.update(sandbox_audit_metadata_from_tool_context(context))
        return ToolResult(
            output=json.dumps(
                {
                    "cwd": sandbox_repo_root_from_tool_context(context),
                    "pattern": pattern,
                    "filenames": list(result.filenames),
                    "num_files": result.num_files,
                    "truncated": result.truncated,
                }
            ),
            metadata=metadata,
        )
    except Exception as exc:
        return ToolResult(
            output=sandbox_path_error_message(exc, resolved_path or pattern) or str(exc),
            is_error=True,
        )


__all__ = ["glob"]
