"""Process-local registry: one ``TaskSegmentManager`` per open ``TaskSegment``."""

from __future__ import annotations

from typing import TYPE_CHECKING

from task_center.exceptions import GraphInvariantViolation

if TYPE_CHECKING:
    from task_center.episode.manager import TaskSegmentManager


class SegmentManagerRegistry:
    """In-memory registry enforcing one-manager-per-open-segment."""

    def __init__(self) -> None:
        self._by_segment_id: dict[str, "TaskSegmentManager"] = {}

    def register(self, manager: "TaskSegmentManager") -> None:
        segment_id = manager.task_segment_id
        if segment_id in self._by_segment_id:
            raise GraphInvariantViolation(
                f"TaskSegmentManager already registered for segment {segment_id!r}"
            )
        self._by_segment_id[segment_id] = manager

    def get(self, task_segment_id: str) -> "TaskSegmentManager | None":
        return self._by_segment_id.get(task_segment_id)

    def deregister(self, task_segment_id: str) -> None:
        self._by_segment_id.pop(task_segment_id, None)
