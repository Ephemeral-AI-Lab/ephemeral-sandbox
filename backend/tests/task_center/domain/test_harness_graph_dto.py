"""Domain DTO tests for HarnessGraph."""

from __future__ import annotations

from datetime import UTC, datetime

from task_center.attempt import (
    HarnessGraph,
    HarnessGraphFailReason,
    HarnessGraphStage,
    HarnessGraphStatus,
)


def _graph(**overrides) -> HarnessGraph:
    base = dict(
        id="g1",
        task_segment_id="s1",
        graph_sequence_no=1,
        stage=HarnessGraphStage.PLANNING,
        status=HarnessGraphStatus.RUNNING,
        planner_task_id=None,
        task_specification=None,
        evaluation_criteria=(),
        generator_task_ids=(),
        evaluator_task_id=None,
        continuation_goal=None,
        fail_reason=None,
        created_at=datetime.now(UTC),
        updated_at=datetime.now(UTC),
        closed_at=None,
    )
    base.update(overrides)
    return HarnessGraph(**base)


def test_has_partial_continuation_matches_continuation_goal():
    assert _graph(continuation_goal=None).has_partial_continuation is False
    assert _graph(continuation_goal="x").has_partial_continuation is True


def test_is_closed_matches_stage():
    assert _graph(stage=HarnessGraphStage.PLANNING).is_closed is False
    assert _graph(stage=HarnessGraphStage.GENERATING).is_closed is False
    assert _graph(stage=HarnessGraphStage.EVALUATING).is_closed is False
    assert _graph(stage=HarnessGraphStage.CLOSED).is_closed is True


def test_fail_reason_enum_values():
    assert (
        HarnessGraphFailReason.PLANNER_FAILED.value
        == "planner_failed"
    )
    assert HarnessGraphFailReason.GENERATOR_FAILED.value == "generator_failed"
    assert HarnessGraphFailReason.EVALUATOR_FAILED.value == "evaluator_failed"
    assert HarnessGraphFailReason.STARTUP_FAILED.value == "startup_failed"
