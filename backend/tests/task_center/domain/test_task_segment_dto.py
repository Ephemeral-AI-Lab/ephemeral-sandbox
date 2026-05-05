"""Domain DTO tests for TaskSegment."""

from __future__ import annotations

from datetime import UTC, datetime

from task_center.episode.episode import (
    TaskSegment,
    TaskSegmentCreationReason,
    TaskSegmentStatus,
)


def _seg(**overrides) -> TaskSegment:
    base = dict(
        id="s1",
        complex_task_request_id="r1",
        sequence_no=1,
        creation_reason=TaskSegmentCreationReason.INITIAL,
        goal="g",
        attempt_budget=2,
        status=TaskSegmentStatus.OPEN,
        harness_graph_ids=(),
        continuation_goal=None,
        created_at=datetime.now(UTC),
        updated_at=datetime.now(UTC),
        closed_at=None,
    )
    base.update(overrides)
    return TaskSegment(**base)


def test_attempt_count_equals_len_of_graph_ids():
    assert _seg(harness_graph_ids=()).attempt_count == 0
    assert _seg(harness_graph_ids=("g1",)).attempt_count == 1
    assert _seg(harness_graph_ids=("g1", "g2")).attempt_count == 2


def test_has_budget_remaining_flips_at_boundary():
    assert _seg(attempt_budget=2, harness_graph_ids=()).has_budget_remaining
    assert _seg(
        attempt_budget=2, harness_graph_ids=("g1",)
    ).has_budget_remaining
    assert not _seg(
        attempt_budget=2, harness_graph_ids=("g1", "g2")
    ).has_budget_remaining


def test_latest_graph_id_returns_last():
    assert _seg().latest_graph_id is None
    assert _seg(harness_graph_ids=("a", "b")).latest_graph_id == "b"


def test_is_open_matches_status():
    assert _seg(status=TaskSegmentStatus.OPEN).is_open
    assert not _seg(status=TaskSegmentStatus.SUCCEEDED).is_open
    assert not _seg(status=TaskSegmentStatus.FAILED).is_open
