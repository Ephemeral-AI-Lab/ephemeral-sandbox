"""Result construction helpers for sandbox API tool verbs."""

from __future__ import annotations

from collections.abc import Mapping
from typing import TypeVar

from sandbox.api._impl._payload import (
    conflict_from_payload,
    int_from_payload,
    paths_from_payload,
    timings_from_payload,
)
from sandbox._shared.models import (
    ConflictInfo,
    EditFileResult,
    GuardedResultBase,
    ReadFileResult,
    ShellResult,
)

TGuarded = TypeVar("TGuarded", bound=GuardedResultBase)


def read_result_from_payload(raw: Mapping[str, object]) -> ReadFileResult:
    return ReadFileResult(
        success=bool(raw.get("success", False)),
        exists=bool(raw.get("exists", False)),
        content=str(raw.get("content", "")),
        encoding=str(raw.get("encoding", "utf-8")),
        timings=timings_from_payload(raw.get("timings")),
    )


def guarded_result_from_payload(
    result_cls: type[TGuarded],
    raw: Mapping[str, object],
    *,
    timings: dict[str, float] | None = None,
    **extra: object,
) -> TGuarded:
    conflict = conflict_from_payload(raw.get("conflict"))
    return result_cls(
        success=bool(raw.get("success", False)),
        changed_paths=paths_from_payload(raw.get("changed_paths")),
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
            else timings_from_payload(raw.get("timings"))
        ),
        **extra,
    )


def shell_result_from_payload(
    raw: Mapping[str, object],
    *,
    timings: dict[str, float],
) -> ShellResult:
    return guarded_result_from_payload(
        ShellResult,
        raw,
        exit_code=int_from_payload(raw.get("exit_code"), default=1),
        stdout=str(raw.get("stdout", "")),
        stderr=str(raw.get("stderr", "")),
        warnings=paths_from_payload(raw.get("warnings")),
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


__all__ = [
    "edit_conflict_result",
    "guarded_result_from_payload",
    "read_result_from_payload",
    "shell_conflict_result",
    "shell_error_result",
    "shell_result_from_payload",
]
