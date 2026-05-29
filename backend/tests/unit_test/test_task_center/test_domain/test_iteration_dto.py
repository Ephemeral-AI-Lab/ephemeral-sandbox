"""Domain DTO tests for Iteration."""

from __future__ import annotations

from datetime import UTC, datetime

from task_center.iteration.state import (
    Iteration,
    IterationCreationReason,
    IterationStatus,
)


def _seg(**overrides) -> Iteration:
    base = dict(
        id="s1",
        workflow_id="r1",
        sequence_no=1,
        creation_reason=IterationCreationReason.INITIAL,
        goal="g",
        attempt_budget=2,
        status=IterationStatus.OPEN,
        attempt_ids=(),
        deferred_goal_for_next_iteration=None,
        created_at=datetime.now(UTC),
        updated_at=datetime.now(UTC),
        closed_at=None,
    )
    base.update(overrides)
    return Iteration(**base)


def test_attempt_count_equals_len_of_attempt_ids():
    assert _seg(attempt_ids=()).attempt_count == 0
    assert _seg(attempt_ids=("g1",)).attempt_count == 1
    assert _seg(attempt_ids=("g1", "g2")).attempt_count == 2


def test_has_budget_remaining_flips_at_boundary():
    assert _seg(attempt_budget=2, attempt_ids=()).has_budget_remaining
    assert _seg(
        attempt_budget=2, attempt_ids=("g1",)
    ).has_budget_remaining
    assert not _seg(
        attempt_budget=2, attempt_ids=("g1", "g2")
    ).has_budget_remaining


def test_latest_attempt_id_returns_last():
    assert _seg().latest_attempt_id is None
    assert _seg(attempt_ids=("a", "b")).latest_attempt_id == "b"


def test_is_open_matches_status():
    assert _seg(status=IterationStatus.OPEN).is_open
    assert not _seg(status=IterationStatus.SUCCEEDED).is_open
    assert not _seg(status=IterationStatus.FAILED).is_open
