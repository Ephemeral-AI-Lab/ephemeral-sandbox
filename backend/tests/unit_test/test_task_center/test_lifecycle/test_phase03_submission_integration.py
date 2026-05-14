"""Phase 03 submission tool integration smoke tests."""

from __future__ import annotations

import pytest

from task_center.attempt import AttemptStatus
from task_center.attempt.orchestrator import AttemptOrchestrator
from task_center.attempt.orchestrator_registry import (
    AttemptOrchestratorRegistry,
)
from task_center.attempt.runtime import AgentLaunch, AttemptDeps
from task_center.episode.registry import EpisodeManagerRegistry
from task_center.episode.episode import EpisodeCreationReason
from task_center.task import evaluator_task_id, generator_task_id, planner_task_id
from tools._framework.core.context import ToolExecutionContextService
from tools._framework.core.runtime import ExecutionMetadata
from tools._framework.execution.tool_call import execute_tool_once
from tools.submission.evaluator import submit_evaluation_success
from tools.submission.executor import submit_execution_success
from tools.submission.planner import submit_full_plan

pytestmark = pytest.mark.asyncio


class _FakeLauncher:
    def __init__(self) -> None:
        self.launches: list[AgentLaunch] = []

    def launch(self, launch: AgentLaunch) -> None:
        self.launches.append(launch)


async def _noop_emit(event) -> None:
    del event


def _tool_context(
    runtime: AttemptDeps,
    attempt_id: str,
    task_id: str,
    *,
    role: str = "executor",
):
    metadata = ExecutionMetadata(
        task_center_task_id=task_id,
        task_center_attempt_id=attempt_id,
        attempt_runtime=runtime,
    )
    metadata["role"] = role
    return ToolExecutionContextService(cwd="/tmp", services=metadata)


def _build_runtime(mission_store, episode_store, attempt_store, task_store, *, composer):
    request = mission_store.insert(
        task_center_run_id="run1",
        requested_by_task_id="outer-task",
        goal="solve task",
    )
    episode = episode_store.insert(
        mission_id=request.id,
        sequence_no=1,
        creation_reason=EpisodeCreationReason.INITIAL,
        goal="solve task",
        attempt_budget=2,
    )
    mission_store.append_episode_id(request.id, episode.id)
    attempt = attempt_store.insert(episode_id=episode.id, attempt_sequence_no=1)
    episode_store.append_attempt_id(episode.id, attempt.id)
    launcher = _FakeLauncher()
    registry = AttemptOrchestratorRegistry()
    runtime = AttemptDeps(
        mission_store=mission_store,
        episode_store=episode_store,
        attempt_store=attempt_store,
        task_store=task_store,
        agent_launcher=launcher,
        orchestrator_registry=registry,
        manager_registry=EpisodeManagerRegistry(),
        composer=composer,
    )
    orchestrator = AttemptOrchestrator(
        attempt=attempt,
        on_attempt_closed=lambda attempt_id: None,
        runtime=runtime,
    )
    registry.register(orchestrator)
    return runtime, orchestrator, attempt.id


async def test_phase03_full_plan_through_evaluator_success(
    mission_store, episode_store, attempt_store, task_store, composer
) -> None:
    runtime, orchestrator, attempt_id = _build_runtime(
        mission_store,
        episode_store,
        attempt_store,
        task_store,
        composer=composer,
    )
    orchestrator.start()

    planner_result = await execute_tool_once(
        submit_full_plan,
        {
            "task_specification": "Implement and verify a change.",
            "evaluation_criteria": ["generator passed"],
            "tasks": [{"id": "a", "agent_name": "executor", "deps": []}],
            "task_specs": {"a": "Do the work."},
        },
        _tool_context(runtime, attempt_id, planner_task_id(attempt_id)),
        emit=_noop_emit,
    )
    generator_result = await execute_tool_once(
        submit_execution_success,
        {"summary": "done", "artifacts": []},
        _tool_context(runtime, attempt_id, generator_task_id(attempt_id, "a")),
        emit=_noop_emit,
    )
    evaluator_result = await execute_tool_once(
        submit_evaluation_success,
        {"summary": "passed", "passed_criteria": ["generator passed"]},
        _tool_context(runtime, attempt_id, evaluator_task_id(attempt_id)),
        emit=_noop_emit,
    )

    attempt = attempt_store.get(attempt_id)
    assert not planner_result.is_error
    assert not generator_result.is_error
    assert not evaluator_result.is_error
    assert attempt is not None
    assert attempt.status == AttemptStatus.PASSED
