"""Invariant tests across request, episode, and attempt levels."""

from __future__ import annotations

from datetime import UTC, datetime

import pytest

from task_center._core.infra import (
    assert_continuation_episode_predecessor,
    assert_mission_open,
    assert_episode_id_unique_in_mission,
    assert_episode_sequence_contiguous,
)
from task_center._core.infra import (
    assert_fail_reason_present_on_failure,
    assert_attempt_sequence_contiguous,
)
from task_center._core.infra import (
    assert_attempt_belongs_to_episode,
    assert_episode_has_budget,
    assert_episode_open,
)
from task_center.episode.registry import EpisodeManagerRegistry
from task_center.mission.state import (
    Mission,
    MissionStatus,
)
from task_center.attempt import (
    Attempt,
    AttemptFailReason,
    AttemptStage,
    AttemptStatus,
)
from task_center.episode.state import (
    Episode,
    EpisodeCreationReason,
    EpisodeStatus,
)
from task_center._core.types import TaskCenterInvariantViolation


def _request(
    status: MissionStatus = MissionStatus.OPEN,
    episode_ids: tuple[str, ...] = (),
) -> Mission:
    now = datetime.now(UTC)
    return Mission(
        id="r1",
        task_center_run_id="run1",
        requested_by_task_id="t1",
        goal="g",
        status=status,
        episode_ids=episode_ids,
        final_outcome=None,
        created_at=now,
        updated_at=now,
        closed_at=None,
    )


def _segment(
    *,
    status: EpisodeStatus = EpisodeStatus.OPEN,
    attempt_ids: tuple[str, ...] = (),
    continuation_goal: str | None = None,
    attempt_budget: int = 2,
    sid: str = "s1",
) -> Episode:
    now = datetime.now(UTC)
    return Episode(
        id=sid,
        mission_id="r1",
        sequence_no=1,
        creation_reason=EpisodeCreationReason.INITIAL,
        goal="g",
        attempt_budget=attempt_budget,
        status=status,
        attempt_ids=attempt_ids,
        continuation_goal=continuation_goal,
        created_at=now,
        updated_at=now,
        closed_at=None,
    )


def _graph(
    *,
    status: AttemptStatus = AttemptStatus.RUNNING,
    fail_reason: AttemptFailReason | None = None,
    episode_id: str = "s1",
    gid: str = "g1",
) -> Attempt:
    now = datetime.now(UTC)
    return Attempt(
        id=gid,
        episode_id=episode_id,
        attempt_sequence_no=1,
        stage=AttemptStage.PLAN,
        status=status,
        planner_task_id=None,
        task_specification=None,
        evaluation_criteria=(),
        generator_task_ids=(),
        evaluator_task_id=None,
        continuation_goal=None,
        fail_reason=fail_reason,
        created_at=now,
        updated_at=now,
        closed_at=None,
    )


# ---- Request-level ------------------------------------------------------


def test_assert_mission_open_passes_for_open():
    assert_mission_open(_request(status=MissionStatus.OPEN))


def test_assert_mission_open_fails_for_closed():
    for status in (
        MissionStatus.SUCCEEDED,
        MissionStatus.FAILED,
        MissionStatus.CANCELLED,
    ):
        with pytest.raises(TaskCenterInvariantViolation):
            assert_mission_open(_request(status=status))


def test_assert_episode_id_unique_in_mission():
    assert_episode_id_unique_in_mission(
        _request(episode_ids=("s1", "s2")), "s3"
    )
    with pytest.raises(TaskCenterInvariantViolation):
        assert_episode_id_unique_in_mission(
            _request(episode_ids=("s1",)), "s1"
        )


def test_assert_episode_sequence_contiguous():
    assert_episode_sequence_contiguous(_request(episode_ids=()), 1)
    assert_episode_sequence_contiguous(_request(episode_ids=("s1",)), 2)
    with pytest.raises(TaskCenterInvariantViolation):
        assert_episode_sequence_contiguous(_request(episode_ids=("s1",)), 1)
    with pytest.raises(TaskCenterInvariantViolation):
        assert_episode_sequence_contiguous(_request(episode_ids=("s1",)), 3)


def test_assert_continuation_episode_predecessor_requires_succeeded_with_goal():
    succeeded_with_goal = _segment(
        status=EpisodeStatus.SUCCEEDED, continuation_goal="next"
    )
    assert_continuation_episode_predecessor(succeeded_with_goal)

    with pytest.raises(TaskCenterInvariantViolation):
        assert_continuation_episode_predecessor(
            _segment(status=EpisodeStatus.OPEN, continuation_goal="next")
        )
    with pytest.raises(TaskCenterInvariantViolation):
        assert_continuation_episode_predecessor(
            _segment(status=EpisodeStatus.SUCCEEDED, continuation_goal=None)
        )


# ---- Segment-level ------------------------------------------------------


def test_assert_episode_open():
    assert_episode_open(_segment(status=EpisodeStatus.OPEN))
    with pytest.raises(TaskCenterInvariantViolation):
        assert_episode_open(_segment(status=EpisodeStatus.SUCCEEDED))


def test_assert_episode_has_budget():
    assert_episode_has_budget(_segment(attempt_budget=2, attempt_ids=()))
    assert_episode_has_budget(
        _segment(attempt_budget=2, attempt_ids=("g1",))
    )
    with pytest.raises(TaskCenterInvariantViolation):
        assert_episode_has_budget(
            _segment(attempt_budget=2, attempt_ids=("g1", "g2"))
        )


def test_assert_attempt_belongs_to_episode():
    assert_attempt_belongs_to_episode(
        _graph(episode_id="s1"), _segment(sid="s1")
    )
    with pytest.raises(TaskCenterInvariantViolation):
        assert_attempt_belongs_to_episode(
            _graph(episode_id="s1"), _segment(sid="s2")
        )


# ---- Graph-level --------------------------------------------------------


def test_assert_attempt_sequence_contiguous():
    assert_attempt_sequence_contiguous(_segment(attempt_ids=()), 1)
    assert_attempt_sequence_contiguous(_segment(attempt_ids=("g1",)), 2)
    with pytest.raises(TaskCenterInvariantViolation):
        assert_attempt_sequence_contiguous(_segment(attempt_ids=("g1",)), 1)


def test_assert_fail_reason_present_on_failure():
    assert_fail_reason_present_on_failure(
        _graph(status=AttemptStatus.PASSED)
    )
    assert_fail_reason_present_on_failure(
        _graph(
            status=AttemptStatus.FAILED,
            fail_reason=AttemptFailReason.GENERATOR_FAILED,
        )
    )
    with pytest.raises(TaskCenterInvariantViolation):
        assert_fail_reason_present_on_failure(
            _graph(status=AttemptStatus.FAILED, fail_reason=None)
        )


# ---- Manager registry ---------------------------------------------------


def test_episode_manager_registry_enforces_uniqueness():
    reg = EpisodeManagerRegistry()

    class _Fake:
        episode_id = "s1"

    reg.register(_Fake())  # type: ignore[arg-type]
    assert reg.get("s1") is not None
    with pytest.raises(TaskCenterInvariantViolation):
        reg.register(_Fake())  # type: ignore[arg-type]
    reg.deregister("s1")
    assert reg.get("s1") is None
