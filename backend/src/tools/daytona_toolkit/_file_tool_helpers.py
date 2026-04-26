"""Shared helpers for file search and read tools."""

from __future__ import annotations

import json
from typing import Any

from pydantic import BaseModel, Field

from code_intelligence.tuning import CODE_INTELLIGENCE_TUNING
from tools.core.base import ToolExecutionContextService, ToolResult
from tools.daytona_toolkit._daytona_utils import (
    _get_repo_root,
    _recover_sandbox,
    _require_sandbox,
    _truncate,
)

_GREP_MATCH_CAP = CODE_INTELLIGENCE_TUNING.grep_match_cap


class ReadFileInput(BaseModel):
    file_path: str = Field(
        ...,
        description="Repo-relative or sandbox-root file path.",
    )
    start_line: int = Field(
        default=1,
        ge=1,
        description="First line to return. Lines are 1-based.",
    )
    end_line: int | None = Field(
        default=None,
        ge=1,
        description="Last line to return. The line is included.",
    )


class ReadFileOutput(BaseModel):
    cwd: str = Field(..., description="Current sandbox working directory.")
    file_path: str = Field(..., description="Resolved file path that was read.")
    total_lines: int = Field(..., description="Total number of lines in the file.")
    start_line: int = Field(..., description="First line returned.")
    end_line: int = Field(..., description="Last line returned.")
    content: str = Field(..., description="Selected file content with line numbers.")


class WriteFileInput(BaseModel):
    file_path: str = Field(
        ...,
        description="Repo-relative or sandbox-root file path.",
    )
    content: str = Field(..., description="Text to write.")


class WriteFileOutput(BaseModel):
    cwd: str = Field(..., description="Current sandbox working directory.")
    file_path: str = Field(..., description="Resolved file path that was written.")
    bytes_written: int = Field(..., description="Number of UTF-8 bytes written.")
    ci_sync: bool = Field(..., description="Whether code intelligence saw the write.")
    warnings: list[str] = Field(default_factory=list, description="Non-fatal write warnings.")
    timings: dict[str, Any] | None = Field(
        default=None,
        description="Optional write timing metadata.",
    )


class GrepInput(BaseModel):
    pattern: str = Field(..., description="Regex pattern to search for in file contents.")
    path: str = Field(
        default=".",
        description="Repo-relative or sandbox-root directory path.",
    )


class MatchOutput(BaseModel):
    file: str = Field(..., description="Matched file path.")
    line: int | None = Field(default=None, description="Matched one-based line number.")
    content: str = Field(..., description="Matched line content.")


class GrepOutput(BaseModel):
    cwd: str = Field(..., description="Current sandbox working directory.")
    pattern: str = Field(..., description="Pattern that was searched.")
    path: str = Field(..., description="Search root path.")
    matches: list[MatchOutput] = Field(
        default_factory=list,
        description="Matching file lines.",
    )
    total_matches: int = Field(..., description="Total number of matches found.")
    truncated: bool = Field(..., description="Whether returned matches were capped.")


class GlobInput(BaseModel):
    pattern: str = Field(..., description="Glob pattern for file names, such as **/*.py.")
    path: str = Field(
        default=".",
        description="Repo-relative or sandbox-root directory path.",
    )


class GlobOutput(BaseModel):
    cwd: str = Field(..., description="Current sandbox working directory.")
    pattern: str = Field(..., description="Glob pattern used.")
    path: str = Field(..., description="Search root path.")
    files: list[str] = Field(default_factory=list, description="Matching file paths.")
    total_files: int = Field(..., description="Total number of matching files.")


async def run_with_recovery(
    context: ToolExecutionContextService,
    operation: Any,
) -> Any:
    """Run an operation once, then retry after sandbox recovery."""
    sandbox = await _require_sandbox(context)
    try:
        return await operation(sandbox)
    except Exception as exc:
        return await operation(await _recover_sandbox(context, exc))


def build_read_file_result(
    *,
    context: ToolExecutionContextService,
    file_path: str,
    content: str,
    start_line: int,
    end_line: int | None,
) -> ToolResult:
    lines = content.splitlines()
    total = len(lines)
    start = max(1, start_line)
    end = min(total, end_line) if end_line else total
    selected = [f"{i:4d}: {lines[i - 1]}" for i in range(start, end + 1)]
    return ToolResult(
        output=json.dumps(
            {
                "cwd": _get_repo_root(context) or "",
                "file_path": file_path,
                "total_lines": total,
                "start_line": start,
                "end_line": end,
                "content": _truncate("\n".join(selected)),
            }
        )
    )


def build_match_result(match: dict[str, Any]) -> dict[str, Any]:
    return {
        "file": str(match.get("file") or ""),
        "line": match.get("line"),
        "content": str(match.get("content") or "").rstrip(),
    }


def build_find_result(
    *,
    cwd: str,
    pattern: str,
    path: str,
    matches: list[dict[str, Any]],
    total_matches: int | None = None,
    truncated: bool = False,
) -> ToolResult:
    total = len(matches) if total_matches is None else int(total_matches)
    return ToolResult(
        output=json.dumps(
            {
                "cwd": cwd,
                "pattern": pattern,
                "path": path,
                "matches": [build_match_result(match) for match in matches[:_GREP_MATCH_CAP]],
                "total_matches": total,
                "truncated": bool(truncated or total > _GREP_MATCH_CAP),
            }
        )
    )


def build_glob_result(
    *,
    cwd: str,
    pattern: str,
    path: str,
    files: list[str],
) -> ToolResult:
    return ToolResult(
        output=json.dumps(
            {
                "cwd": cwd,
                "pattern": pattern,
                "path": path,
                "files": files,
                "total_files": len(files),
            }
        )
    )


__all__ = [
    "GlobInput",
    "GlobOutput",
    "GrepInput",
    "GrepOutput",
    "ReadFileInput",
    "ReadFileOutput",
    "WriteFileInput",
    "WriteFileOutput",
    "_GREP_MATCH_CAP",
    "build_find_result",
    "build_glob_result",
    "build_read_file_result",
    "run_with_recovery",
]
