"""Payload projection helpers for sandbox-local guarded daemon operations."""

from __future__ import annotations

from collections.abc import Iterable

from sandbox.models import ConflictInfo
from sandbox.timing import normalize_timing_map

_INTERNAL_ERROR_PREFIX = "internal_error: "


def normalize_overlay_cwd(cwd: str | None) -> str:
    """Normalize public shell cwd values for overlay execution."""
    normalized = (cwd or "").strip()
    return normalized or "."


def error_message(error: BaseException) -> str:
    message = str(getattr(error, "message", "") or error)
    if message.startswith(_INTERNAL_ERROR_PREFIX):
        return message.removeprefix(_INTERNAL_ERROR_PREFIX)
    return message


def conflict_from_payload(raw: object) -> ConflictInfo | None:
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


def paths_from_payload(raw: object) -> tuple[str, ...]:
    if not isinstance(raw, Iterable) or isinstance(raw, (str, bytes, dict)):
        return ()
    return tuple(str(path) for path in raw if str(path or "").strip())


def timings_from_payload(raw: object) -> dict[str, float]:
    if not isinstance(raw, dict):
        return {}
    return normalize_timing_map(raw)


def int_from_payload(value: object, *, default: int) -> int:
    if value is None:
        return default
    if isinstance(value, bool):
        raise TypeError(f"expected integer value, got bool ({value!r})")
    if isinstance(value, int):
        return value
    raise TypeError(f"expected integer value, got {type(value).__name__}")


__all__ = [
    "conflict_from_payload",
    "error_message",
    "int_from_payload",
    "normalize_overlay_cwd",
    "paths_from_payload",
    "timings_from_payload",
]
