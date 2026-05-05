"""Direct tests for failed attempt landscape helper behavior."""

from __future__ import annotations

from datetime import UTC, datetime

from task_center.context_engine.packet import ContextPriority
from task_center.context_engine.recipes.attempt_landscape import (
    MAX_FAILED_ATTEMPTS_RENDERED,
    failed_attempt_landscape_blocks,
)
from task_center.attempt import (
    HarnessGraph,
    HarnessGraphFailReason,
    HarnessGraphStage,
    HarnessGraphStatus,
)


def _graph(
    sequence_no: int,
    *,
    graph_id: str | None = None,
    status: HarnessGraphStatus = HarnessGraphStatus.FAILED,
    task_specification: str | None = None,
    evaluation_criteria: tuple[str, ...] = (),
    fail_reason: HarnessGraphFailReason | None = None,
) -> HarnessGraph:
    now = datetime.now(UTC)
    return HarnessGraph(
        id=graph_id or f"graph-{sequence_no}",
        task_segment_id="seg-1",
        graph_sequence_no=sequence_no,
        stage=HarnessGraphStage.CLOSED,
        status=status,
        planner_task_id=None,
        task_specification=task_specification,
        evaluation_criteria=evaluation_criteria,
        generator_task_ids=(),
        evaluator_task_id=None,
        continuation_goal=None,
        fail_reason=fail_reason,
        created_at=now,
        updated_at=now,
        closed_at=now,
    )


def test_excludes_current_graph_even_if_current_is_failed():
    current = _graph(
        3,
        graph_id="current",
        task_specification="current spec",
        evaluation_criteria=("current crit",),
        fail_reason=HarnessGraphFailReason.PLANNER_FAILED,
    )
    blocks = failed_attempt_landscape_blocks(
        current_graph_id=current.id,
        graphs=[
            current,
            _graph(
                2,
                task_specification="older spec",
                evaluation_criteria=("older crit",),
                fail_reason=HarnessGraphFailReason.GENERATOR_FAILED,
            ),
            _graph(4, status=HarnessGraphStatus.RUNNING),
            _graph(
                1,
                task_specification="oldest spec",
                evaluation_criteria=("oldest crit",),
                fail_reason=HarnessGraphFailReason.EVALUATOR_FAILED,
            ),
        ],
    )

    assert [block.source_id for block in blocks] == ["graph-1", "graph-2"]
    assert all(block.priority == ContextPriority.HIGH for block in blocks)


def test_renders_missing_spec_empty_criteria_and_unknown_reason():
    blocks = failed_attempt_landscape_blocks(
        current_graph_id=None,
        graphs=[_graph(1)],
    )

    assert len(blocks) == 1
    assert "task_specification: (missing)" in blocks[0].text
    assert "evaluation_criteria:\n  (none)" in blocks[0].text
    assert "fail_reason: unknown" in blocks[0].text


def test_truncation_keeps_most_recent_failed_attempts_and_reports_omitted_range():
    blocks = failed_attempt_landscape_blocks(
        current_graph_id=None,
        graphs=[
            _graph(
                sequence_no,
                task_specification=f"spec-{sequence_no}",
                evaluation_criteria=(f"crit-{sequence_no}",),
                fail_reason=HarnessGraphFailReason.GENERATOR_FAILED,
            )
            for sequence_no in range(MAX_FAILED_ATTEMPTS_RENDERED + 2, 0, -1)
        ],
    )

    rendered = blocks[:-1]
    truncation = blocks[-1]

    assert [block.metadata["graph_sequence_no"] for block in rendered] == [
        str(sequence_no)
        for sequence_no in range(
            3, MAX_FAILED_ATTEMPTS_RENDERED + 3
        )
    ]
    assert all(block.priority == ContextPriority.HIGH for block in rendered)
    assert truncation.priority == ContextPriority.MEDIUM
    assert truncation.metadata["truncated_count"] == "2"
    assert "graph_sequence_no 1-2" in truncation.text
