"""Payload projection helpers for sandbox-local guarded daemon operations."""

from __future__ import annotations

from collections.abc import Iterable

from sandbox.contract import ConflictInfo


def conflict_from_payload(raw: object) -> ConflictInfo | None:
    if not isinstance(raw, dict):
        return None
    return ConflictInfo(
        reason=str(raw.get("reason", "")),
        conflict_file=(
            str(raw.get("conflict_file"))
            if raw.get("conflict_file") is not None
            else None
        ),
        message=str(raw.get("message", "")),
    )


def paths_from_payload(raw: object) -> tuple[str, ...]:
    if not isinstance(raw, Iterable) or isinstance(raw, (str, bytes)):
        return ()
    return tuple(str(path) for path in raw if str(path or "").strip())


def timings_from_payload(raw: object) -> dict[str, float]:
    if not isinstance(raw, dict):
        return {}
    return {str(key): float(value) for key, value in raw.items()}


def int_from_payload(value: object, *, default: int) -> int:
    if value is None:
        return default
    if isinstance(value, (str, int, float)):
        return int(value)
    raise TypeError(f"expected integer value, got {type(value).__name__}")


__all__ = [
    "conflict_from_payload",
    "int_from_payload",
    "paths_from_payload",
    "timings_from_payload",
]
