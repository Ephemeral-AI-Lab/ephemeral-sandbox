"""Classify daemon errors into public sandbox API conflict statuses."""

from __future__ import annotations

from collections.abc import Mapping

from sandbox.audit.conflict_markers import (
    EDIT_CONFLICT_MARKERS as _EDIT_CONFLICT_MARKERS,
    SHELL_CONFLICT_MARKERS as _SHELL_CONFLICT_MARKERS,
)
from sandbox.api.tool._daemon_results import user_visible_error_message

_EDIT_CONFLICT_CODES = {
    "aborted_overlap",
    "anchor_not_found",
    "anchor_occurrence_count_mismatch",
    "old_text_not_found",
}
_SHELL_CONFLICT_CODES = {
    "overlay_escape",
    "rejected_symlink",
    "unsupported_symlink_change",
}


def _structured_error_code(error: BaseException) -> str | None:
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
    code = _structured_error_code(error)
    if code in _EDIT_CONFLICT_CODES:
        return True
    lowered = user_visible_error_message(error).lower()
    return any(marker in lowered for marker in _EDIT_CONFLICT_MARKERS)


def is_shell_conflict(error: BaseException) -> bool:
    code = _structured_error_code(error)
    if code in _SHELL_CONFLICT_CODES:
        return True
    lowered = user_visible_error_message(error).lower()
    return any(marker in lowered for marker in _SHELL_CONFLICT_MARKERS)
