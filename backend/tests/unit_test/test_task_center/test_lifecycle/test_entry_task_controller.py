"""EntryTaskController unit tests."""

from __future__ import annotations

import pytest

from task_center.entry.controller import EntryTaskController
from task_center._core.types import TaskCenterInvariantViolation
from task_center.mission.state import MissionClosureReport
from task_center.task_state import TaskCenterTaskRole, TaskCenterTaskStatus


def _seed_entry_task(*, task_store, task_center_run_id: str) -> str:
    entry_task_id = f"{task_center_run_id}:entry"
    task_store.upsert_task(
        task_id=entry_task_id,
        task_center_run_id=task_center_run_id,
        role=TaskCenterTaskRole.GENERATOR.value,
        agent_name="entry_executor",
        rendered_prompt="entry goal",
        status=TaskCenterTaskStatus.RUNNING.value,
        summaries=[],
        needs=[],
        task_center_attempt_id=None,
        spawn_reason="entry_executor",
    )
    return entry_task_id


@pytest.fixture
def entry_setup(task_store, task_center_run_id):
    entry_task_id = _seed_entry_task(
        task_store=task_store,
        task_center_run_id=task_center_run_id,
    )
    controller = EntryTaskController(
        task_id=entry_task_id,
        task_center_run_id=task_center_run_id,
        task_store=task_store,
    )
    return controller


def test_apply_executor_success_marks_task_and_run_done(
    entry_setup, task_store, task_center_run_id
):
    controller = entry_setup

    controller.apply_executor_success(summary="all good", artifacts=["a.md"])

    task = task_store.get_task(controller.task_id)
    run = task_store.get_run(task_center_run_id)
    assert task is not None
    assert task["status"] == TaskCenterTaskStatus.DONE.value
    assert task["summaries"][-1]["summary"] == "all good"
    assert run is not None
    assert run["status"] == "done"


def test_apply_executor_failure_marks_task_and_run_failed(
    entry_setup, task_store, task_center_run_id
):
    controller = entry_setup

    controller.apply_executor_failure(
        summary="cannot proceed",
        reason="missing input",
        details=["no goal"],
    )

    task = task_store.get_task(controller.task_id)
    run = task_store.get_run(task_center_run_id)
    assert task is not None
    assert task["status"] == TaskCenterTaskStatus.FAILED.value
    assert task["summaries"][-1]["payload"]["reason"] == "missing input"
    assert run is not None
    assert run["status"] == "failed"


def test_apply_run_exhausted_marks_failed(
    entry_setup, task_store, task_center_run_id
):
    controller = entry_setup

    controller.apply_run_exhausted(summary="agent ran without terminal")

    task = task_store.get_task(controller.task_id)
    run = task_store.get_run(task_center_run_id)
    assert task is not None
    assert task["status"] == TaskCenterTaskStatus.FAILED.value
    assert task["summaries"][-1]["fail_reason"] == "run_exhausted"
    assert run is not None
    assert run["status"] == "failed"


def test_mark_waiting_then_closure_report_success(
    entry_setup, task_store, task_center_run_id
):
    controller = entry_setup
    controller.mark_waiting_mission(
        delegated_mission_id="delegated-1",
        delegated_episode_id="delegated-episode",
        delegated_attempt_id="delegated-attempt",
        goal="solve x",
    )
    task = task_store.get_task(controller.task_id)
    assert task is not None
    assert task["status"] == TaskCenterTaskStatus.WAITING_MISSION.value

    controller.apply_mission_closure_report(
        MissionClosureReport(
            mission_id="delegated-1",
            requested_by_task_id=controller.task_id,
            outcome="success",
            final_episode_id="delegated-episode",
            final_attempt_id="delegated-attempt",
        )
    )

    task = task_store.get_task(controller.task_id)
    run = task_store.get_run(task_center_run_id)
    assert task is not None
    assert task["status"] == TaskCenterTaskStatus.DONE.value
    assert run is not None
    assert run["status"] == "done"


def test_closure_report_failure_marks_failed(
    entry_setup, task_store, task_center_run_id
):
    controller = entry_setup
    controller.mark_waiting_mission(
        delegated_mission_id="delegated-1",
        delegated_episode_id="delegated-episode",
        delegated_attempt_id="delegated-attempt",
        goal="solve x",
    )

    controller.apply_mission_closure_report(
        MissionClosureReport(
            mission_id="delegated-1",
            requested_by_task_id=controller.task_id,
            outcome="failed",
            final_episode_id="delegated-episode",
            final_attempt_id="delegated-attempt",
        )
    )

    task = task_store.get_task(controller.task_id)
    run = task_store.get_run(task_center_run_id)
    assert task is not None
    assert task["status"] == TaskCenterTaskStatus.FAILED.value
    assert run is not None
    assert run["status"] == "failed"


def test_closure_report_idempotent_when_task_already_terminal(
    entry_setup, task_store, task_center_run_id
):
    controller = entry_setup
    controller.apply_executor_success(summary="done", artifacts=[])

    controller.apply_mission_closure_report(
        MissionClosureReport(
            mission_id="delegated-1",
            requested_by_task_id=controller.task_id,
            outcome="failed",
            final_episode_id="delegated-episode",
            final_attempt_id=None,
        )
    )

    task = task_store.get_task(controller.task_id)
    run = task_store.get_run(task_center_run_id)
    assert task is not None
    assert task["status"] == TaskCenterTaskStatus.DONE.value
    assert run is not None
    assert run["status"] == "done"


def test_mark_waiting_rejects_when_task_is_not_running(entry_setup, task_store):
    controller = entry_setup
    task_store.set_task_status(
        controller.task_id, status=TaskCenterTaskStatus.DONE.value
    )

    with pytest.raises(TaskCenterInvariantViolation):
        controller.mark_waiting_mission(
            delegated_mission_id="r",
            delegated_episode_id="s",
            delegated_attempt_id="g",
            goal="g",
        )


def test_restore_running_after_failed_mission_start_rolls_back_waiting(
    entry_setup, task_store
):
    controller = entry_setup
    controller.mark_waiting_mission(
        delegated_mission_id="r",
        delegated_episode_id="s",
        delegated_attempt_id="g",
        goal="g",
    )

    controller.restore_running_after_failed_mission_start()

    task = task_store.get_task(controller.task_id)
    assert task is not None
    assert task["status"] == TaskCenterTaskStatus.RUNNING.value


def test_terminal_is_idempotent(entry_setup, task_store, task_center_run_id):
    controller = entry_setup

    controller.apply_executor_success(summary="ok", artifacts=[])
    controller.apply_executor_success(summary="ok again", artifacts=[])

    task = task_store.get_task(controller.task_id)
    run = task_store.get_run(task_center_run_id)
    assert task is not None
    assert task["status"] == TaskCenterTaskStatus.DONE.value
    assert len(task["summaries"]) == 1
    assert run is not None
    assert run["status"] == "done"
