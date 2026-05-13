"""Payload projection helpers for sandbox-local guarded daemon operations."""

from __future__ import annotations

from collections.abc import Iterable

from sandbox.models import ConflictInfo, SandboxCaller


def caller_envelope(caller: SandboxCaller) -> dict[str, str]:
    """Project a SandboxCaller into the daemon-envelope ``caller`` dict.

    Forwards every audit-relevant field so the daemon can stitch runs by
    ``run_id`` / ``agent_run_id`` / ``task_id`` instead of only ``agent_id``.
    """
    envelope = {
        "agent_id": caller.agent_id,
        "run_id": caller.run_id,
        "agent_run_id": caller.agent_run_id,
        "task_id": caller.task_id,
    }
    for key, value in (
        ("task_center_run_id", caller.task_center_run_id),
        ("task_center_task_id", caller.task_center_task_id),
        ("task_center_attempt_id", caller.task_center_attempt_id),
        ("task_center_mission_id", caller.task_center_mission_id),
        ("task_center_request_id", caller.task_center_request_id),
        ("tool_name", caller.tool_name),
        ("tool_id", caller.tool_id),
    ):
        if value:
            envelope[key] = value
    return envelope


def conflict_from_payload(raw: object) -> ConflictInfo | None:
    if not isinstance(raw, dict):
        return None
    conflict_file = raw.get("conflict_file")
    return ConflictInfo(
        reason=str(raw.get("reason", "")),
        # Only stringify the conflict_file when the daemon actually sent a
        # string-coercible primitive. Blindly stringifying arbitrary objects
        # (lists, dicts) yields nonsense paths like "['x', 'y']".
        conflict_file=(
            str(conflict_file)
            if isinstance(conflict_file, (str, int, float, bytes))
            else None
        ),
        message=str(raw.get("message", "")),
    )


def paths_from_payload(raw: object) -> tuple[str, ...]:
    # Reject dict — iterating yields keys with the same surface type as a
    # list of paths, which papers over upstream contract breakage. Bytes
    # and str are excluded to keep the API focused on iterables of paths.
    if (
        not isinstance(raw, Iterable)
        or isinstance(raw, (str, bytes, dict))
    ):
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
        try:
            return int(value)
        except ValueError as exc:
            # The docstring promises TypeError for unparseable values; the
            # bare ``int("abc")`` would surface ValueError and the caller's
            # ``except TypeError`` would not catch it.
            raise TypeError(
                f"expected integer-coercible value, got {value!r}"
            ) from exc
    raise TypeError(f"expected integer value, got {type(value).__name__}")


__all__ = [
    "caller_envelope",
    "conflict_from_payload",
    "int_from_payload",
    "paths_from_payload",
    "timings_from_payload",
]
