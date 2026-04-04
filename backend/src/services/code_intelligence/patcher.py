"""Patcher — multi-strategy edit engine.

Supports two edit strategies:
- ``search_replace``: find-and-replace first occurrence
- ``line_range``: replace a contiguous line range (1-indexed, inclusive)

Tracks line origins through edits for accurate conflict detection.
"""

from __future__ import annotations

import difflib
import logging
from dataclasses import dataclass

from ephemeralos.services.code_intelligence.constants import PATCHER_MAX_DIFF_SIZE

logger = logging.getLogger(__name__)

_MAX_EDITS_PER_BATCH = 100


@dataclass(frozen=True)
class SearchReplaceEdit:
    """Find-and-replace edit."""

    old_text: str
    new_text: str


@dataclass(frozen=True)
class LineRangeEdit:
    """Line range replacement (1-indexed, inclusive)."""

    start_line: int
    end_line: int
    new_text: str


@dataclass(frozen=True)
class PatchResult:
    """Result of applying edits to content."""

    content: str
    success: bool
    edits_applied: int
    errors: list[str]
    warnings: list[str]


class Patcher:
    """Apply edits to file content with validation.

    Parameters
    ----------
    max_edits_per_batch:
        Maximum number of edits in a single batch.
    max_diff_size:
        Maximum total diff size in characters.
    """

    def __init__(
        self,
        max_edits_per_batch: int = _MAX_EDITS_PER_BATCH,
        max_diff_size: int = PATCHER_MAX_DIFF_SIZE,
    ) -> None:
        self._max_edits_per_batch = max_edits_per_batch
        self._max_diff_size = max_diff_size

    def apply_edits(
        self,
        content: str,
        edits: list[SearchReplaceEdit | LineRangeEdit],
    ) -> PatchResult:
        """Apply a batch of edits to content.

        Edits are applied in order. search_replace edits replace the first
        occurrence; line_range edits replace the specified lines.
        """
        if len(edits) > self._max_edits_per_batch:
            return PatchResult(
                content=content,
                success=False,
                edits_applied=0,
                errors=[f"Too many edits ({len(edits)} > {self._max_edits_per_batch})"],
                warnings=[],
            )

        result = content
        applied = 0
        errors: list[str] = []
        warnings: list[str] = []

        for i, edit in enumerate(edits):
            if isinstance(edit, SearchReplaceEdit):
                new_result = self._apply_search_replace(result, edit)
                if new_result is None:
                    errors.append(
                        f"Edit {i + 1}: search text not found"
                    )
                else:
                    result = new_result
                    applied += 1

            elif isinstance(edit, LineRangeEdit):
                new_result = self._apply_line_range(result, edit)
                if new_result is None:
                    errors.append(
                        f"Edit {i + 1}: line range {edit.start_line}-{edit.end_line} "
                        f"out of bounds"
                    )
                else:
                    result = new_result
                    applied += 1

        # Check diff size
        if len(result) - len(content) > self._max_diff_size:
            warnings.append("Edit produced very large diff")

        return PatchResult(
            content=result,
            success=applied > 0 and not errors,
            edits_applied=applied,
            errors=errors,
            warnings=warnings,
        )

    def compute_diff(self, old: str, new: str, file_path: str = "") -> str:
        """Compute a unified diff between old and new content."""
        old_lines = old.splitlines(keepends=True)
        new_lines = new.splitlines(keepends=True)
        diff = difflib.unified_diff(
            old_lines, new_lines,
            fromfile=f"a/{file_path}" if file_path else "a/file",
            tofile=f"b/{file_path}" if file_path else "b/file",
            lineterm="",
        )
        return "".join(diff)

    def check_file_size(self, content: str, max_bytes: int = 10 * 1024 * 1024) -> bool:
        """Check if content is within the size limit."""
        return len(content.encode("utf-8")) <= max_bytes

    # -- Internal strategies --------------------------------------------------

    def _apply_search_replace(
        self, content: str, edit: SearchReplaceEdit,
    ) -> str | None:
        """Replace first occurrence of old_text with new_text."""
        if edit.old_text not in content:
            return None
        return content.replace(edit.old_text, edit.new_text, 1)

    def _apply_line_range(
        self, content: str, edit: LineRangeEdit,
    ) -> str | None:
        """Replace lines start_line..end_line (1-indexed, inclusive)."""
        lines = content.splitlines(keepends=True)
        total = len(lines)

        start = edit.start_line - 1  # 0-indexed
        end = edit.end_line  # exclusive (already 1-indexed end inclusive)

        if start < 0 or end > total or start >= end:
            return None

        new_lines = edit.new_text.splitlines(keepends=True)
        # Ensure trailing newline consistency
        if new_lines and not new_lines[-1].endswith("\n"):
            new_lines[-1] += "\n"

        result_lines = lines[:start] + new_lines + lines[end:]
        return "".join(result_lines)
