"""ComplexTaskRequest-layer invariants. All raise ``GraphInvariantViolation``."""

from __future__ import annotations

from task_center.mission.mission import ComplexTaskRequest
from task_center.exceptions import GraphInvariantViolation
from task_center.episode.episode import TaskSegment, TaskSegmentStatus


def assert_mission_request_open(request: ComplexTaskRequest) -> None:
    if not request.is_open:
        raise GraphInvariantViolation(
            f"ComplexTaskRequest {request.id!r} is not open (status={request.status})"
        )


def assert_episode_id_unique_in_mission(
    request: ComplexTaskRequest, segment_id: str
) -> None:
    if segment_id in request.task_segment_ids:
        raise GraphInvariantViolation(
            f"TaskSegment {segment_id!r} already present in request "
            f"{request.id!r} segment list"
        )


def assert_episode_sequence_contiguous(
    request: ComplexTaskRequest, new_sequence_no: int
) -> None:
    expected = len(request.task_segment_ids) + 1
    if new_sequence_no != expected:
        raise GraphInvariantViolation(
            f"TaskSegment sequence_no must be contiguous: expected {expected}, "
            f"got {new_sequence_no}"
        )


def assert_continuation_episode_predecessor(previous: TaskSegment) -> None:
    if previous.status != TaskSegmentStatus.SUCCEEDED:
        raise GraphInvariantViolation(
            f"Continuation requires predecessor segment {previous.id!r} to be "
            f"SUCCEEDED, not {previous.status}"
        )
    if previous.continuation_goal is None:
        raise GraphInvariantViolation(
            f"Continuation requires predecessor segment {previous.id!r} to have a "
            f"continuation_goal; none was recorded"
        )
