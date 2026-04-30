"""Change resolution and merge policy for semantic writes."""

from __future__ import annotations

from sandbox.code_intelligence.core.hashing import content_hash
from sandbox.code_intelligence.core.types import OperationChange
from sandbox.code_intelligence.mutations.merge import (
    detect_edit_window,
    merge_non_overlapping_edit,
)
from sandbox.code_intelligence.mutations.write_coordinator.models import ResolvedChange


class ChangeResolver:
    """Resolves planned file changes against locked current file state."""

    def resolve_change(
        self,
        change: OperationChange,
        current_now: str,
        existed_now: bool,
    ) -> tuple[ResolvedChange | None, tuple[str, str] | None]:
        current_hash = content_hash(current_now) if existed_now else ""

        if change.final_content is None:
            if not existed_now or current_hash != change.base_hash:
                return None, ("aborted_version", "file content changed before delete")
            return (
                ResolvedChange(
                    change=change,
                    current_content=current_now,
                    final_content=None,
                    current_hash=current_hash,
                    existed=existed_now,
                ),
                None,
            )

        if not change.base_existed:
            if existed_now:
                return None, (
                    "aborted_version",
                    "file already exists; base said it did not",
                )
            return (
                ResolvedChange(
                    change=change,
                    current_content=current_now,
                    final_content=change.final_content,
                    current_hash="",
                    existed=False,
                ),
                None,
            )

        if existed_now and current_hash == change.base_hash:
            return (
                ResolvedChange(
                    change=change,
                    current_content=current_now,
                    final_content=change.final_content,
                    current_hash=current_hash,
                    existed=existed_now,
                ),
                None,
            )

        if change.strict_base:
            return None, (
                "aborted_version",
                "file content changed since base was captured (strict_base=True)",
            )

        resolved_content, conflict = self.resolve_semantic_change(
            change,
            current_now,
            existed_now,
        )
        if conflict is not None:
            return None, conflict
        return (
            ResolvedChange(
                change=change,
                current_content=current_now,
                final_content=resolved_content,
                current_hash=current_hash,
                existed=existed_now,
            ),
            None,
        )

    @staticmethod
    def merge_against_base(
        base_content: str | None,
        final_content: str,
        current_content: str | None,
    ) -> tuple[str | None, str]:
        if base_content is None or current_content is None:
            return None, "missing"
        line_start, line_end, op = detect_edit_window(base_content, final_content)
        if line_start is None:
            return None, "unwindowable"
        merged = merge_non_overlapping_edit(
            original_content=base_content,
            new_content=final_content,
            current_content=current_content,
            line_start=line_start,
            line_end=line_end,
            operation_type=op,
        )
        if merged is None:
            return None, "overlap"
        return merged, ""

    def resolve_semantic_change(
        self,
        change: OperationChange,
        current_now: str,
        existed_now: bool,
    ) -> tuple[str, tuple[str, str] | None]:
        if not existed_now:
            return "", (
                "aborted_version",
                "file was deleted since operation plan was built",
            )
        assert change.final_content is not None
        merged, reason_kind = self.merge_against_base(
            change.base_content,
            change.final_content,
            current_now,
        )
        if reason_kind == "":
            assert merged is not None
            return merged, None
        if reason_kind == "overlap":
            return "", (
                "aborted_overlap",
                "concurrent edit overlaps the operation window",
            )
        return "", (
            "aborted_version",
            "base content changed and rewrite is whole-file / un-windowable",
        )
