"""Parse daemon response payloads into public sandbox API result models."""

from __future__ import annotations

from collections.abc import Iterable, Mapping
from typing import Any, TypeVar, cast

from sandbox._shared.clock import normalize_timing_map
from sandbox._shared.models import (
    ConflictInfo,
    GlobResult,
    GrepResult,
    GuardedResultBase,
    ReadFileResult,
    SandboxRequestBase,
)

TGuarded = TypeVar("TGuarded", bound=GuardedResultBase)
_DAEMON_INTERNAL_ERROR_PREFIX = "internal_error: "


def daemon_request_identity_fields(request: SandboxRequestBase) -> dict[str, object]:
    payload: dict[str, object] = {
        "agent_id": request.caller.agent_id,
        "caller": request.caller.audit_fields(),
    }
    if request.invocation_id:
        payload["invocation_id"] = request.invocation_id
    return payload


def user_visible_error_message(error: BaseException) -> str:
    message = str(getattr(error, "message", "") or error)
    if message.startswith(_DAEMON_INTERNAL_ERROR_PREFIX):
        return message.removeprefix(_DAEMON_INTERNAL_ERROR_PREFIX)
    return message


def parse_conflict_info_field(raw: object) -> ConflictInfo | None:
    if not isinstance(raw, dict):
        return None
    conflict_file = raw.get("conflict_file")
    return ConflictInfo(
        reason=str(raw.get("reason", "")),
        conflict_file=(
            str(conflict_file)
            if isinstance(conflict_file, (str, int, float, bytes))
            else None
        ),
        message=str(raw.get("message", "")),
    )


def parse_path_tuple_field(raw: object) -> tuple[str, ...]:
    if not isinstance(raw, Iterable) or isinstance(raw, (str, bytes, dict)):
        return ()
    return tuple(str(path) for path in raw if str(path or "").strip())


def parse_timing_map_field(raw: object) -> dict[str, float]:
    if not isinstance(raw, dict):
        return {}
    return normalize_timing_map(raw)


def strict_int_from_daemon_field(value: object, *, default: int) -> int:
    """Return an integer boundary value without accepting bool-as-int."""
    if value is None:
        return default
    if isinstance(value, bool):
        raise TypeError(f"expected integer value, got bool ({value!r})")
    if isinstance(value, int):
        return value
    raise TypeError(f"expected integer value, got {type(value).__name__}")


def parse_read_file_result(response: Mapping[str, object]) -> ReadFileResult:
    return ReadFileResult(
        success=bool(response.get("success", False)),
        exists=bool(response.get("exists", False)),
        content=str(response.get("content", "")),
        encoding=str(response.get("encoding", "utf-8")),
        timings=parse_timing_map_field(response.get("timings")),
    )


def parse_glob_result(response: Mapping[str, object]) -> GlobResult:
    return GlobResult(
        success=bool(response.get("success", False)),
        filenames=parse_path_tuple_field(response.get("filenames")),
        num_files=strict_int_from_daemon_field(response.get("num_files"), default=0),
        truncated=bool(response.get("truncated", False)),
        timings=parse_timing_map_field(response.get("timings")),
    )


def parse_grep_result(
    response: Mapping[str, object],
) -> GrepResult:
    applied_limit_raw = response.get("applied_limit")
    applied_limit = (
        strict_int_from_daemon_field(applied_limit_raw, default=0)
        if applied_limit_raw is not None
        else None
    )
    return GrepResult(
        success=bool(response.get("success", False)),
        output_mode=str(response.get("output_mode", "files_with_matches")),
        filenames=parse_path_tuple_field(response.get("filenames")),
        content=str(response.get("content", "")),
        num_files=strict_int_from_daemon_field(response.get("num_files"), default=0),
        num_lines=strict_int_from_daemon_field(response.get("num_lines"), default=0),
        num_matches=strict_int_from_daemon_field(response.get("num_matches"), default=0),
        applied_limit=applied_limit,
        applied_offset=strict_int_from_daemon_field(
            response.get("applied_offset"), default=0
        ),
        truncated=bool(response.get("truncated", False)),
        timings=parse_timing_map_field(response.get("timings")),
    )


def parse_guarded_mutation_result(
    result_cls: type[TGuarded],
    response: Mapping[str, object],
    *,
    timings: dict[str, float] | None = None,
    **extra: object,
) -> TGuarded:
    conflict = parse_conflict_info_field(response.get("conflict"))
    error_payload = response.get("error")
    return result_cls(
        success=bool(response.get("success", False)),
        changed_paths=parse_path_tuple_field(response.get("changed_paths")),
        changed_path_kinds=parse_changed_path_kinds_field(
            response.get("changed_path_kinds")
        ),
        mutation_source=str(response.get("mutation_source") or ""),
        status=str(response.get("status", "")),
        conflict=conflict,
        conflict_reason=(
            str(response.get("conflict_reason"))
            if response.get("conflict_reason") is not None
            else None
        ),
        error=dict(error_payload) if isinstance(error_payload, dict) else None,
        timings=(
            timings
            if timings is not None
            else parse_timing_map_field(response.get("timings"))
        ),
        **cast(Any, extra),
    )


def parse_changed_path_kinds_field(raw: object) -> dict[str, str]:
    if not isinstance(raw, Mapping):
        return {}
    return {
        str(path): str(kind)
        for path, kind in raw.items()
        if str(path or "").strip() and str(kind or "").strip()
    }
