"""US-009: EpisodeStore.close_succeeded atomicity + denormalization."""

from __future__ import annotations

from datetime import UTC, datetime

import pytest

from task_center.episode.episode import (
    EpisodeCreationReason,
    EpisodeStatus,
)


def _seed_segment(mission_store, episode_store, task_center_run_id):
    req = mission_store.insert(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="t-entry",
        goal="g",
    )
    return episode_store.insert(
        mission_id=req.id,
        sequence_no=1,
        creation_reason=EpisodeCreationReason.INITIAL,
        goal="g",
        attempt_budget=2,
    )


def test_close_succeeded_writes_status_spec_summary_atomically(
    mission_store, episode_store, task_center_run_id
):
    seg = _seed_segment(mission_store, episode_store, task_center_run_id)
    closed = episode_store.close_succeeded(
        seg.id,
        task_specification="resulting spec",
        task_summary="evaluator pass summary",
        closed_at=datetime.now(UTC),
    )
    assert closed.status == EpisodeStatus.SUCCEEDED
    assert closed.task_specification == "resulting spec"
    assert closed.task_summary == "evaluator pass summary"
    assert closed.closed_at is not None


def test_close_succeeded_persists_through_get(
    mission_store, episode_store, task_center_run_id
):
    seg = _seed_segment(mission_store, episode_store, task_center_run_id)
    episode_store.close_succeeded(
        seg.id,
        task_specification="spec",
        task_summary="summary",
    )
    reloaded = episode_store.get(seg.id)
    assert reloaded is not None
    assert reloaded.task_specification == "spec"
    assert reloaded.task_summary == "summary"


def test_failed_close_leaves_denormalized_fields_null(
    mission_store, episode_store, task_center_run_id
):
    seg = _seed_segment(mission_store, episode_store, task_center_run_id)
    failed = episode_store.set_status(
        seg.id,
        status=EpisodeStatus.FAILED,
        closed_at=datetime.now(UTC),
    )
    assert failed.status == EpisodeStatus.FAILED
    assert failed.task_specification is None
    assert failed.task_summary is None


def test_close_succeeded_unknown_segment_raises(episode_store):
    with pytest.raises(LookupError):
        episode_store.close_succeeded(
            "no-such-episode",
            task_specification="x",
            task_summary="y",
        )


def test_initial_episode_has_null_denormalized_fields(
    mission_store, episode_store, task_center_run_id
):
    seg = _seed_segment(mission_store, episode_store, task_center_run_id)
    assert seg.task_specification is None
    assert seg.task_summary is None


def test_evaluator_pass_summary_helper(
    task_store, task_center_run_id
):
    """TaskCenterStore.get_evaluator_pass_summary fetches the latest text."""
    attempt_id = "g-1"
    task_store.upsert_task(
        task_id="ev-1",
        task_center_run_id=task_center_run_id,
        role="evaluator",
        agent_name="evaluator",
        rendered_prompt="x",
        status="running",
        summaries=[],
        needs=[],
        task_center_attempt_id=attempt_id,
        spawn_reason="attempt_evaluator",
    )
    task_store.set_task_status(
        "ev-1",
        status="done",
        summary={
            "outcome": "success",
            "summary": "all evaluation criteria passed",
            "payload": {},
        },
    )
    assert (
        task_store.get_evaluator_pass_summary(attempt_id)
        == "all evaluation criteria passed"
    )


def test_evaluator_pass_summary_missing_returns_empty_string(
    task_store,
):
    assert task_store.get_evaluator_pass_summary("nonexistent-attempt") == ""
