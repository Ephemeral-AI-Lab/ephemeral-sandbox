"""Mission / episode context block builders shared by role recipes."""

from __future__ import annotations

from task_center.mission.mission import ComplexTaskRequest
from task_center.context_engine.errors import ContextEngineError
from task_center.context_engine.packet import (
    ContextBlock,
    ContextBlockKind,
    ContextPriority,
)
from task_center.episode.episode import TaskSegment

MISSION_EPISODE_HEADING = "# Mission / Current Episode"
MISSION_HEADING = "# Mission"
CURRENT_EPISODE_HEADING = "# Current Episode"
PREVIOUS_EPISODE_RESULTS_HEADING = "# Previous Episode Results"


def mission_episode_blocks(
    *,
    request: ComplexTaskRequest,
    current_segment: TaskSegment,
    segments: list[TaskSegment],
) -> list[ContextBlock]:
    """Return the mission/episode frame in LLM-facing semantic order."""
    if current_segment.sequence_no == 1:
        return [_episode_goal_block(current_segment, heading=MISSION_EPISODE_HEADING)]

    return [
        _mission_goal_block(request),
        *_previous_episode_result_blocks(
            current=current_segment,
            segments=segments,
        ),
        _episode_goal_block(
            current_segment,
            heading=CURRENT_EPISODE_HEADING,
        ),
    ]


def _episode_goal_block(segment: TaskSegment, *, heading: str) -> ContextBlock:
    return ContextBlock(
        kind=ContextBlockKind.SEGMENT_GOAL,
        priority=ContextPriority.REQUIRED,
        text=segment.goal,
        source_id=segment.id,
        source_kind="task_segment",
        metadata={"heading": heading},
    )


def _mission_goal_block(request: ComplexTaskRequest) -> ContextBlock:
    return ContextBlock(
        kind=ContextBlockKind.COMPLEX_TASK_GOAL,
        priority=ContextPriority.REQUIRED,
        text=request.goal,
        source_id=request.id,
        source_kind="complex_task_request",
        metadata={"heading": MISSION_HEADING},
    )


def _previous_episode_result_blocks(
    *,
    current: TaskSegment,
    segments: list[TaskSegment],
) -> list[ContextBlock]:
    priors = sorted(
        (s for s in segments if s.sequence_no < current.sequence_no),
        key=lambda s: s.sequence_no,
    )
    out: list[ContextBlock] = []
    immediate_prior_sequence = current.sequence_no - 1
    for prior in priors:
        if prior.task_specification is None or prior.task_summary is None:
            raise ContextEngineError(
                f"Prior segment {prior.id!r} (seq={prior.sequence_no}) is "
                "missing task_specification or task_summary; chain "
                "integrity violated."
            )
        priority = (
            ContextPriority.HIGH
            if prior.sequence_no == immediate_prior_sequence
            else ContextPriority.MEDIUM
        )
        base_meta = {
            "episode_sequence_no": str(prior.sequence_no),
            "group_heading": PREVIOUS_EPISODE_RESULTS_HEADING,
        }
        out.append(
            ContextBlock(
                kind=ContextBlockKind.PRIOR_SEGMENT_SPECIFICATION,
                priority=priority,
                text=prior.task_specification,
                source_id=prior.id,
                source_kind="task_segment",
                metadata={
                    **base_meta,
                    "segment_sequence_no": str(prior.sequence_no),
                    "subheading": f"Episode {prior.sequence_no} accepted plan",
                },
            )
        )
        out.append(
            ContextBlock(
                kind=ContextBlockKind.PRIOR_SEGMENT_SUMMARY,
                priority=priority,
                text=prior.task_summary,
                source_id=prior.id,
                source_kind="task_segment",
                metadata={
                    **base_meta,
                    "segment_sequence_no": str(prior.sequence_no),
                    "subheading": f"Episode {prior.sequence_no} summary",
                },
            )
        )
    return out


__all__ = [
    "CURRENT_EPISODE_HEADING",
    "MISSION_EPISODE_HEADING",
    "MISSION_HEADING",
    "PREVIOUS_EPISODE_RESULTS_HEADING",
    "mission_episode_blocks",
]
