"""Regression tests for TaskCenter agent launcher scheduling."""

from __future__ import annotations

import asyncio
from types import SimpleNamespace

import pytest

from task_center.attempt.launch import EphemeralAttemptAgentLauncher
from task_center.attempt import AttemptFailReason, AttemptStatus
from task_center.attempt.orchestrator_registry import AttemptOrchestratorRegistry
from task_center.attempt.deps import AgentLaunch, AttemptDeps
from task_center.iteration.state import IterationCreationReason
from task_center._core.task_state import TaskCenterTaskRole, TaskCenterTaskStatus
from task_center._core.primitives import planner_task_id


class _NoopLauncher:
    def launch(self, launch: AgentLaunch) -> None:
        del launch


@pytest.mark.asyncio
async def test_wait_for_idle_prunes_done_tasks_before_next_loop() -> None:
    launcher = EphemeralAttemptAgentLauncher(
        config=SimpleNamespace(),
        deps_provider=lambda: None,
    )
    done_task = asyncio.create_task(asyncio.sleep(0))
    await done_task
    launcher._pending.add(done_task)  # noqa: SLF001 - regression seam

    await asyncio.wait_for(launcher.wait_for_idle(), timeout=0.2)

    assert launcher._pending == set()  # noqa: SLF001 - regression seam


@pytest.mark.asyncio
async def test_missing_orchestrator_exhaustion_closes_attempt(
    workflow_store, iteration_store, attempt_store, task_store, task_center_run_id
) -> None:
    goal = workflow_store.insert(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="outer-task",
        goal="solve",
    )
    iteration = iteration_store.insert(
        workflow_id=goal.id,
        sequence_no=1,
        creation_reason=IterationCreationReason.INITIAL,
        goal="solve",
        attempt_budget=1,
    )
    workflow_store.append_iteration_id(goal.id, iteration.id)
    attempt = attempt_store.insert(iteration_id=iteration.id, attempt_sequence_no=1)
    iteration_store.append_attempt_id(iteration.id, attempt.id)
    task_id = planner_task_id(attempt.id)
    task_store.upsert_task(
        task_id=task_id,
        task_center_run_id=task_center_run_id,
        role=TaskCenterTaskRole.PLANNER.value,
        agent_name="planner",
        context_message="plan",
        status=TaskCenterTaskStatus.RUNNING.value,
        summaries=[],
        needs=[],
        task_center_attempt_id=attempt.id,
        spawn_reason="attempt_planner",
    )
    runtime = AttemptDeps(
        workflow_store=workflow_store,
        iteration_store=iteration_store,
        attempt_store=attempt_store,
        task_store=task_store,
        agent_launcher=_NoopLauncher(),
        orchestrator_registry=AttemptOrchestratorRegistry(),
    )
    launcher = EphemeralAttemptAgentLauncher(
        config=SimpleNamespace(),
        deps_provider=lambda: runtime,
    )

    await launcher._report_unfinished_running_task(  # noqa: SLF001 - regression seam
        AgentLaunch(
            task_id=task_id,
            task_center_run_id=task_center_run_id,
            attempt_id=attempt.id,
            role=TaskCenterTaskRole.PLANNER,
            agent_name="planner",
            context="plan",
            task_guidance="plan the work",
            needs=(),
            workflow_id=goal.id,
        ),
        summary="Agent run ended without a terminal submission.",
    )

    task = task_store.get_task(task_id)
    refreshed = attempt_store.get(attempt.id)
    assert task is not None
    assert task["status"] == TaskCenterTaskStatus.FAILED.value
    assert refreshed is not None
    assert refreshed.status == AttemptStatus.FAILED
    assert refreshed.fail_reason == AttemptFailReason.PLANNER_FAILED
