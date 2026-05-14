"""Client-side error classification for public sandbox API verbs."""

from __future__ import annotations

from collections.abc import Mapping

from sandbox.api._impl._payload import error_message

_EDIT_CONFLICT_CODES = {
    "aborted_overlap",
    "anchor_not_found",
    "anchor_occurrence_count_mismatch",
    "old_text_not_found",
}
_EDIT_CONFLICT_MARKERS = (
    "anchor not found",
    "anchor occurrence count mismatch",
    "aborted_overlap",
    "old_text_not_found",
)
_SHELL_CONFLICT_CODES = {
    "overlay_escape",
    "rejected_symlink",
    "unsupported_symlink_change",
}
_SHELL_CONFLICT_MARKERS = (
    "overlay capture refuses escaping symlink target",
    "unsupported tracked change kind: symlinkchange",
)


def _error_code(error: BaseException) -> str | None:
    for attr in ("error_code", "code", "reason"):
        value = getattr(error, attr, None)
        if isinstance(value, str) and value.strip():
            return value.strip().lower()
    details = getattr(error, "details", None)
    if isinstance(details, Mapping):
        value = details.get("code") or details.get("error_code") or details.get("reason")
        if isinstance(value, str) and value.strip():
            return value.strip().lower()
    return None


def is_edit_conflict(error: BaseException) -> bool:
    code = _error_code(error)
    if code in _EDIT_CONFLICT_CODES:
        return True
    lowered = error_message(error).lower()
    return any(marker in lowered for marker in _EDIT_CONFLICT_MARKERS)


def is_shell_conflict(error: BaseException) -> bool:
    code = _error_code(error)
    if code in _SHELL_CONFLICT_CODES:
        return True
    lowered = error_message(error).lower()
    return any(marker in lowered for marker in _SHELL_CONFLICT_MARKERS)


__all__ = ["is_edit_conflict", "is_shell_conflict"]
