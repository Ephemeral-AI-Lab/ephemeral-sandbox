"""Domain DTO tests for Attempt."""

from __future__ import annotations

from datetime import UTC, datetime

from task_center.attempt import (
    Attempt,
    AttemptFailReason,
    AttemptStage,
    AttemptStatus,
)


def _graph(**overrides) -> Attempt:
    base = dict(
        id="g1",
        episode_id="s1",
        attempt_sequence_no=1,
        stage=AttemptStage.PLAN,
        status=AttemptStatus.RUNNING,
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
    return Attempt(**base)


def test_has_partial_continuation_matches_continuation_goal():
    assert _graph(continuation_goal=None).has_partial_continuation is False
    assert _graph(continuation_goal="x").has_partial_continuation is True


def test_is_closed_matches_stage():
    assert _graph(stage=AttemptStage.PLAN).is_closed is False
    assert _graph(stage=AttemptStage.GENERATE).is_closed is False
    assert _graph(stage=AttemptStage.EVALUATE).is_closed is False
    assert _graph(stage=AttemptStage.CLOSED).is_closed is True


def test_fail_reason_enum_values():
    assert (
        AttemptFailReason.PLANNER_FAILED.value
        == "planner_failed"
    )
    assert AttemptFailReason.GENERATOR_FAILED.value == "generator_failed"
    assert AttemptFailReason.EVALUATOR_FAILED.value == "evaluator_failed"
    assert AttemptFailReason.STARTUP_FAILED.value == "startup_failed"
