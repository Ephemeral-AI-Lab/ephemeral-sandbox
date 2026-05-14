"""Persistence tests for AttemptStore."""

from __future__ import annotations

from task_center.attempt import (
    Attempt,
    AttemptFailReason,
    AttemptStage,
    AttemptStatus,
)
from task_center.episode.episode import EpisodeCreationReason


def _seed_segment(
    mission_store, episode_store, task_center_run_id, sequence_no=1
) -> str:
    req = mission_store.insert(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="t1",
        goal="g",
    )
    seg = episode_store.insert(
        mission_id=req.id,
        sequence_no=sequence_no,
        creation_reason=EpisodeCreationReason.INITIAL,
        goal="g",
        attempt_budget=2,
    )
    return seg.id


def test_insert_returns_running_planning_dto(
    attempt_store, episode_store, mission_store, task_center_run_id
):
    seg_id = _seed_segment(mission_store, episode_store, task_center_run_id)
    g = attempt_store.insert(episode_id=seg_id, attempt_sequence_no=1)
    assert isinstance(g, Attempt)
    assert g.stage == AttemptStage.PLAN
    assert g.status == AttemptStatus.RUNNING
    assert g.evaluation_criteria == ()
    assert g.generator_task_ids == ()
    assert g.fail_reason is None


def test_set_plan_contract_persists_fields(
    attempt_store, episode_store, mission_store, task_center_run_id
):
    seg_id = _seed_segment(mission_store, episode_store, task_center_run_id)
    g = attempt_store.insert(episode_id=seg_id, attempt_sequence_no=1)
    g = attempt_store.set_plan_contract(
        g.id,
        task_specification="spec",
        evaluation_criteria=["c1", "c2"],
        continuation_goal="next",
    )
    assert g.task_specification == "spec"
    assert g.evaluation_criteria == ("c1", "c2")
    assert g.continuation_goal == "next"


def test_close_records_status_fail_reason_and_closed_at(
    attempt_store, episode_store, mission_store, task_center_run_id
):
    seg_id = _seed_segment(mission_store, episode_store, task_center_run_id)
    g = attempt_store.insert(episode_id=seg_id, attempt_sequence_no=1)
    closed = attempt_store.close(
        g.id,
        status=AttemptStatus.FAILED,
        fail_reason=AttemptFailReason.EVALUATOR_FAILED,
    )
    assert closed.is_closed
    assert closed.status == AttemptStatus.FAILED
    assert closed.fail_reason == AttemptFailReason.EVALUATOR_FAILED
    assert closed.closed_at is not None


def test_list_for_episode_orders_by_attempt_sequence_no(
    attempt_store, episode_store, mission_store, task_center_run_id
):
    seg_id = _seed_segment(mission_store, episode_store, task_center_run_id)
    g2 = attempt_store.insert(episode_id=seg_id, attempt_sequence_no=2)
    g1 = attempt_store.insert(episode_id=seg_id, attempt_sequence_no=1)
    listed = attempt_store.list_for_episode(seg_id)
    assert [g.id for g in listed] == [g1.id, g2.id]


def test_get_by_sequence(
    attempt_store, episode_store, mission_store, task_center_run_id
):
    seg_id = _seed_segment(mission_store, episode_store, task_center_run_id)
    g = attempt_store.insert(episode_id=seg_id, attempt_sequence_no=1)
    found = attempt_store.get_by_sequence(
        episode_id=seg_id, attempt_sequence_no=1
    )
    assert found is not None and found.id == g.id
    missing = attempt_store.get_by_sequence(
        episode_id=seg_id, attempt_sequence_no=99
    )
    assert missing is None
