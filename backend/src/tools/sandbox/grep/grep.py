"""Grep tool: regex-scan workspace file contents."""

from __future__ import annotations

import json
from typing import Literal

from pydantic import BaseModel, ConfigDict, Field

import sandbox.api as sandbox_api
from sandbox._shared.models import Intent
from sandbox.api import GrepRequest
from tools._framework.core.base import ToolExecutionContextService, ToolResult
from tools._framework.core.decorator import tool
from .prompt import get_grep_description
from tools.sandbox._lib.tool_context import (
    sandbox_audit_kwargs_from_tool_context,
    sandbox_caller_from_tool_context,
    sandbox_repo_root_from_tool_context,
    sandbox_path_error_message,
    resolve_tool_sandbox_path,
    sandbox_audit_metadata_from_tool_context,
    sandbox_id_or_missing_error_result,
)


class GrepInput(BaseModel):
    model_config = ConfigDict(extra="forbid")

    pattern: str = Field(
        ...,
        description=(
            "Python `re` regex pattern (NOT PCRE2). Possessive quantifiers and "
            "recursive groups are unsupported."
        ),
    )
    path: str | None = Field(
        default=None,
        description=(
            "Optional workspace-relative directory to restrict scanning to."
        ),
    )
    glob_filter: str | None = Field(
        default=None,
        description=(
            "Optional fnmatch glob restricting which file paths are scanned "
            "(e.g. '*.py' to skip non-Python files)."
        ),
    )
    output_mode: Literal["content", "files_with_matches", "count"] = Field(
        default="files_with_matches",
        description=(
            "'files_with_matches' (default): list of files containing matches. "
            "'count': files with per-file match counts. "
            "'content': matched lines formatted 'path:line:body' (or 'path:body' "
            "when line_numbers=False)."
        ),
    )
    head_limit: int = Field(
        default=250,
        ge=0,
        description=(
            "Truncate result set after this many entries (files in matches/count "
            "modes; lines in content mode). Set to 0 for unlimited (subject to "
            "20 KB content cap)."
        ),
    )
    offset: int = Field(
        default=0,
        ge=0,
        description="Skip the first N matches (pagination helper).",
    )
    case_insensitive: bool = Field(
        default=False,
        description="Apply re.IGNORECASE.",
    )
    line_numbers: bool = Field(
        default=False,
        description="In content mode, prefix each line with its line number.",
    )
    multiline: bool = Field(
        default=False,
        description=(
            "When true, apply re.MULTILINE | re.DOTALL — '.' matches newlines "
            "and ^/$ match line boundaries."
        ),
    )


class GrepOutput(BaseModel):
    cwd: str = Field(..., description="Current sandbox working directory.")
    pattern: str = Field(..., description="Regex pattern that was applied.")
    mode: str = Field(..., description="Output mode in effect.")
    filenames: list[str] = Field(
        default_factory=list,
        description="Files containing matches, in scan order.",
    )
    content: str = Field(
        default="",
        description=(
            "Rendered match content for 'content' mode, or 'path:count' lines "
            "for 'count' mode. Empty in 'files_with_matches' mode."
        ),
    )
    num_files: int = Field(default=0, description="Number of files with matches.")
    num_lines: int = Field(default=0, description="Number of lines emitted (content mode).")
    num_matches: int = Field(default=0, description="Total regex matches counted.")
    applied_limit: int | None = Field(
        default=None,
        description="Head limit actually applied (None when unlimited).",
    )
    applied_offset: int = Field(default=0, description="Offset actually applied.")
    truncated: bool = Field(
        default=False,
        description="True when head_limit or the 20 KB content cap was hit.",
    )


@tool(
    name="grep",
    description=get_grep_description(),
    short_description="Regex-search workspace file contents.",
    input_model=GrepInput,
    output_model=GrepOutput,
    intent=Intent.READ_ONLY,
)
async def grep(
    pattern: str,
    path: str | None = None,
    glob_filter: str | None = None,
    output_mode: str = "files_with_matches",
    head_limit: int = 250,
    offset: int = 0,
    case_insensitive: bool = False,
    line_numbers: bool = False,
    multiline: bool = False,
    *,
    context: ToolExecutionContextService,
) -> ToolResult:
    """Search file contents in the workspace snapshot."""
    # output_mode is validated by Pydantic via the Literal type on
    # GrepInput.output_mode — no runtime re-check needed here.
    resolved_path = resolve_tool_sandbox_path(path, context) if path else None
    sandbox_id, sandbox_id_error = sandbox_id_or_missing_error_result(context)
    if sandbox_id_error is not None:
        return sandbox_id_error
    try:
        result = await sandbox_api.grep(
            sandbox_id,
            GrepRequest(
                pattern=pattern,
                path=resolved_path,
                glob_filter=glob_filter,
                output_mode=output_mode,
                # Pass-through: ``head_limit=0`` is the documented "unlimited"
                # sentinel and must reach the daemon as 0 (the daemon's
                # ``if head_limit and ...`` checks treat 0 as unlimited).
                # Mapping 0 → None here would let the API wrapper drop the
                # key from the payload, causing the daemon to fall back to
                # the 250-entry default and silently truncate.
                head_limit=head_limit,
                offset=offset,
                case_insensitive=case_insensitive,
                line_numbers=line_numbers,
                multiline=multiline,
                caller=sandbox_caller_from_tool_context(context),
            ),
            **sandbox_audit_kwargs_from_tool_context(context),
        )
        if not result.success:
            raise RuntimeError(f"grep failed for pattern: {pattern}")
        metadata: dict[str, object] = {}
        if result.timings:
            metadata["timings"] = dict(result.timings)
        metadata.update(sandbox_audit_metadata_from_tool_context(context))
        return ToolResult(
            output=json.dumps(
                {
                    "cwd": sandbox_repo_root_from_tool_context(context),
                    "pattern": pattern,
                    "mode": result.output_mode,
                    "filenames": list(result.filenames),
                    "content": result.content,
                    "num_files": result.num_files,
                    "num_lines": result.num_lines,
                    "num_matches": result.num_matches,
                    "applied_limit": result.applied_limit,
                    "applied_offset": result.applied_offset,
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


__all__ = ["grep"]
