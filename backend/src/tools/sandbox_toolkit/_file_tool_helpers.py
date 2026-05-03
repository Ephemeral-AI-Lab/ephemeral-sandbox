"""Shared helpers for file search and read tools."""

from __future__ import annotations

import json
from typing import Any

from pydantic import BaseModel, Field, model_validator

from tools.core.base import ToolExecutionContextService, ToolResult
from tools.core.sandbox_session import get_repo_root


MAX_READ_FILE_LINES = 200


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
    end_line: int = Field(
        default=MAX_READ_FILE_LINES,
        ge=1,
        description=(
            "Last line to return, inclusive. Omit this field to read up to 200 "
            "lines from start_line; do not pass null."
        ),
    )

    @model_validator(mode="before")
    @classmethod
    def default_end_line_to_window(cls, data: Any) -> Any:
        if not isinstance(data, dict) or "end_line" in data:
            return data
        start_line = data.get("start_line", 1)
        if not isinstance(start_line, int):
            return data
        return {**data, "end_line": start_line + MAX_READ_FILE_LINES - 1}

    @model_validator(mode="after")
    def validate_line_range(self) -> "ReadFileInput":
        if self.end_line < self.start_line:
            raise ValueError("end_line cannot be smaller than start_line")
        if self.end_line - self.start_line + 1 > MAX_READ_FILE_LINES:
            raise ValueError(f"read_file can return at most {MAX_READ_FILE_LINES} lines")
        return self


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
    status: str = Field(..., description="Write result: written, aborted_version, or failed.")
    changed_paths: list[str] = Field(default_factory=list, description="Files changed by the write.")
    conflict_reason: str | None = Field(default=None, description="Conflict reason when write failed.")
    bytes_written: int = Field(..., description="Number of UTF-8 bytes written.")


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


def build_read_file_result(
    *,
    context: ToolExecutionContextService,
    file_path: str,
    content: str,
    start_line: int,
    end_line: int,
) -> ToolResult:
    lines = content.splitlines()
    total = len(lines)
    start = max(1, start_line)
    end = min(total, end_line, start + MAX_READ_FILE_LINES - 1)
    selected = [f"{i:4d}: {lines[i - 1]}" for i in range(start, end + 1)]
    return ToolResult(
        output=json.dumps(
            {
                "cwd": get_repo_root(context),
                "file_path": file_path,
                "total_lines": total,
                "start_line": start,
                "end_line": end,
                "content": "\n".join(selected),
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
) -> ToolResult:
    total = len(matches) if total_matches is None else int(total_matches)
    return ToolResult(
        output=json.dumps(
            {
                "cwd": cwd,
                "pattern": pattern,
                "path": path,
                "matches": [build_match_result(match) for match in matches],
                "total_matches": total,
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
    "MAX_READ_FILE_LINES",
    "ReadFileInput",
    "ReadFileOutput",
    "WriteFileInput",
    "WriteFileOutput",
    "build_find_result",
    "build_glob_result",
    "build_read_file_result",
]
