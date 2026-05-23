"""Build public sandbox API results from daemon response payloads."""

from __future__ import annotations

from collections.abc import Mapping
from typing import TypeVar

from sandbox.api.tool.core.daemon_response import (
    conflict_from_daemon_response,
    int_from_daemon_response,
    paths_from_daemon_response,
    timings_from_daemon_response,
)
from sandbox._shared.models import (
    ConflictInfo,
    EditFileResult,
    GlobResult,
    GrepResult,
    GuardedResultBase,
    ReadFileResult,
    ShellResult,
)

TGuarded = TypeVar("TGuarded", bound=GuardedResultBase)


def read_result_from_daemon_response(raw: Mapping[str, object]) -> ReadFileResult:
    return ReadFileResult(
        success=bool(raw.get("success", False)),
        exists=bool(raw.get("exists", False)),
        content=str(raw.get("content", "")),
        encoding=str(raw.get("encoding", "utf-8")),
        timings=timings_from_daemon_response(raw.get("timings")),
    )


def glob_result_from_daemon_response(raw: Mapping[str, object]) -> GlobResult:
    return GlobResult(
        success=bool(raw.get("success", False)),
        filenames=paths_from_daemon_response(raw.get("filenames")),
        num_files=int_from_daemon_response(raw.get("num_files"), default=0),
        truncated=bool(raw.get("truncated", False)),
        timings=timings_from_daemon_response(raw.get("timings")),
    )


def grep_result_from_daemon_response(
    raw: Mapping[str, object],
) -> GrepResult:
    applied_limit_raw = raw.get("applied_limit")
    applied_limit = (
        int_from_daemon_response(applied_limit_raw, default=0)
        if applied_limit_raw is not None
        else None
    )
    return GrepResult(
        success=bool(raw.get("success", False)),
        output_mode=str(raw.get("output_mode", "files_with_matches")),
        filenames=paths_from_daemon_response(raw.get("filenames")),
        content=str(raw.get("content", "")),
        num_files=int_from_daemon_response(raw.get("num_files"), default=0),
        num_lines=int_from_daemon_response(raw.get("num_lines"), default=0),
        num_matches=int_from_daemon_response(raw.get("num_matches"), default=0),
        applied_limit=applied_limit,
        applied_offset=int_from_daemon_response(raw.get("applied_offset"), default=0),
        truncated=bool(raw.get("truncated", False)),
        timings=timings_from_daemon_response(raw.get("timings")),
    )


def guarded_result_from_daemon_response(
    result_cls: type[TGuarded],
    raw: Mapping[str, object],
    *,
    timings: dict[str, float] | None = None,
    **extra: object,
) -> TGuarded:
    conflict = conflict_from_daemon_response(raw.get("conflict"))
    return result_cls(
        success=bool(raw.get("success", False)),
        changed_paths=paths_from_daemon_response(raw.get("changed_paths")),
        status=str(raw.get("status", "")),
        conflict=conflict,
        conflict_reason=(
            str(raw.get("conflict_reason"))
            if raw.get("conflict_reason") is not None
            else None
        ),
        timings=(
            timings
            if timings is not None
            else timings_from_daemon_response(raw.get("timings"))
        ),
        **extra,
    )


def shell_result_from_daemon_response(
    raw: Mapping[str, object],
    *,
    timings: dict[str, float],
) -> ShellResult:
    return guarded_result_from_daemon_response(
        ShellResult,
        raw,
        exit_code=int_from_daemon_response(raw.get("exit_code"), default=1),
        stdout=str(raw.get("stdout", "")),
        stderr=str(raw.get("stderr", "")),
        warnings=paths_from_daemon_response(raw.get("warnings")),
        timings=timings,
    )


def edit_conflict_result(path: str, message: str) -> EditFileResult:
    return EditFileResult(
        success=False,
        changed_paths=(path,),
        applied_edits=0,
        status="aborted_overlap",
        conflict=ConflictInfo.overlap(path=path, message=message),
        conflict_reason=message,
        timings={},
    )


def shell_conflict_result(
    message: str,
    *,
    timings: dict[str, float],
) -> ShellResult:
    return ShellResult(
        success=False,
        exit_code=1,
        stdout="",
        stderr="",
        changed_paths=(),
        status="rejected",
        conflict=ConflictInfo.rejected(message=message),
        conflict_reason=message,
        warnings=(),
        timings=timings,
    )


def shell_error_result(
    *,
    reason: str,
    message: str,
    timings: dict[str, float] | None = None,
) -> ShellResult:
    return ShellResult(
        success=False,
        exit_code=1,
        stdout="",
        stderr="",
        changed_paths=(),
        status="error",
        conflict=ConflictInfo.rejected(reason=reason, message=message),
        conflict_reason=message,
        warnings=(),
        timings=timings or {},
    )
