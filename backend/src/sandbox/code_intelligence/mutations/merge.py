"""Helpers for merging non-overlapping stale edits to the same file."""

from __future__ import annotations


def detect_edit_window(
    original_content: str,
    new_content: str,
) -> tuple[int | None, int | None, str]:
    """Return the minimal changed half-open line window plus operation type.

    The returned range uses 1-indexed half-open coordinates: ``[line_start, line_end)``.
    """
    original_lines = original_content.splitlines(keepends=True)
    new_lines = new_content.splitlines(keepends=True)

    min_len = min(len(original_lines), len(new_lines))
    first_diff: int | None = None
    for index in range(min_len):
        if original_lines[index] != new_lines[index]:
            first_diff = index
            break

    if first_diff is None:
        if len(original_lines) == len(new_lines):
            return None, None, "replace"
        first_diff = min_len

    last_diff_original = len(original_lines)
    last_diff_new = len(new_lines)
    while last_diff_original > first_diff and last_diff_new > first_diff:
        if original_lines[last_diff_original - 1] != new_lines[last_diff_new - 1]:
            break
        last_diff_original -= 1
        last_diff_new -= 1

    original_range = last_diff_original - first_diff
    new_range = last_diff_new - first_diff
    if new_range > original_range:
        operation_type = "insert"
    elif new_range < original_range:
        operation_type = "delete"
    else:
        operation_type = "replace"

    return first_diff + 1, last_diff_original, operation_type


def merge_non_overlapping_edit(
    *,
    original_content: str,
    new_content: str,
    current_content: str,
    line_start: int,
    line_end: int | None,
    operation_type: str,
) -> str | None:
    """Merge a stale edit into *current_content* when its target range is unchanged."""
    original_lines = original_content.splitlines(keepends=True)
    current_lines = current_content.splitlines(keepends=True)
    new_lines = new_content.splitlines(keepends=True)

    start = line_start - 1
    end = line_end if line_end is not None else len(original_lines)
    if (
        start < 0
        or end > len(original_lines)
        or end > len(current_lines)
        or start > len(new_lines)
    ):
        return None

    # The stale edit must not modify content outside its declared window.
    if original_lines[:start] != new_lines[:start]:
        return None
    if original_lines[start:end] != current_lines[start:end]:
        return None

    if operation_type == "replace":
        if end > len(new_lines) or start > len(new_lines):
            return None
        replacement_slice = new_lines[start:end]
        new_end_for_post = end
    elif operation_type == "insert":
        delta = len(new_lines) - len(original_lines)
        new_end = end + delta
        if start > len(new_lines) or new_end > len(new_lines) or new_end < start:
            return None
        replacement_slice = new_lines[start:new_end]
        new_end_for_post = new_end
    elif operation_type == "delete":
        delta = len(original_lines) - len(new_lines)
        new_end = max(start, end - delta)
        if start > len(new_lines) or new_end > len(new_lines):
            return None
        replacement_slice = new_lines[start:new_end]
        new_end_for_post = new_end
    else:
        return None

    if original_lines[end:] != new_lines[new_end_for_post:]:
        return None

    merged_lines = current_lines[:start] + replacement_slice + current_lines[end:]
    return "".join(merged_lines)
