"""Shared helpers for Phase 03 submission tool tests."""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
from typing import Any

from task_center.attempt.orchestrator import AttemptOrchestrator
from task_center.attempt.orchestrator_registry import (
    AttemptOrchestratorRegistry,
)
from task_center.attempt.runtime import AgentLaunch, AttemptDeps
from task_center.episode.registry import EpisodeManagerRegistry
from task_center.episode.episode import EpisodeCreationReason
from task_center.task import (
    GeneratorSubmission,
    PlannedGeneratorTask,
    PlannerSubmission,
    evaluator_task_id,
    generator_task_id,
    planner_task_id,
)
from tools._framework.core.context import ToolExecutionContextService
from tools._framework.core.runtime import ExecutionMetadata


@dataclass
class TaskCenterFixture:
    runtime: AttemptDeps
    orchestrator: AttemptOrchestrator
    attempt_id: str
    request_id: str
    episode_id: str


class FakeLauncher:
    def __init__(self) -> None:
        self.launches: list[AgentLaunch] = []

    def launch(self, launch: AgentLaunch) -> None:
        self.launches.append(launch)


def build_harness_fixture(
    *,
    mission_store: Any,
    episode_store: Any,
    attempt_store: Any,
    task_store: Any,
    composer: Any,
) -> TaskCenterFixture:
    request = mission_store.insert(
        task_center_run_id="run1",
        requested_by_task_id="outer-task",
        goal="solve the task",
    )
    episode = episode_store.insert(
        mission_id=request.id,
        sequence_no=1,
        creation_reason=EpisodeCreationReason.INITIAL,
        goal="solve the task",
        attempt_budget=2,
    )
    mission_store.append_episode_id(request.id, episode.id)
    attempt = attempt_store.insert(episode_id=episode.id, attempt_sequence_no=1)
    episode_store.append_attempt_id(episode.id, attempt.id)

    launcher = FakeLauncher()
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
    return TaskCenterFixture(
        runtime=runtime,
        orchestrator=orchestrator,
        attempt_id=attempt.id,
        request_id=request.id,
        episode_id=episode.id,
    )


def make_tool_context(
    fixture: TaskCenterFixture,
    task_id: str,
    *,
    messages: list[Any] | None = None,
    role: str | None = "executor",
    agent_type: str | None = None,
) -> ToolExecutionContextService:
    metadata = ExecutionMetadata(
        task_center_task_id=task_id,
        task_center_attempt_id=fixture.attempt_id,
        attempt_runtime=fixture.runtime,
        conversation_messages=list(messages or []),
    )
    if role is not None:
        metadata["role"] = role
    if agent_type is not None:
        metadata["agent_type"] = agent_type
    return ToolExecutionContextService(cwd=Path("/tmp"), services=metadata)


def start_planner(fixture: TaskCenterFixture) -> str:
    fixture.orchestrator.start()
    return planner_task_id(fixture.attempt_id)


def apply_single_generator_plan(fixture: TaskCenterFixture, *, agent_name: str = "executor") -> str:
    planner_id = start_planner(fixture)
    fixture.orchestrator.apply_plan_submission(
        PlannerSubmission(
            attempt_id=fixture.attempt_id,
            planner_task_id=planner_id,
            kind="full",
            task_specification="spec",
            evaluation_criteria=("criterion",),
            tasks=(
                PlannedGeneratorTask(
                    local_id="a",
                    agent_name=agent_name,
                    deps=(),
                    task_spec="do A",
                ),
            ),
            continuation_goal=None,
            summary="plan",
        )
    )
    return generator_task_id(fixture.attempt_id, "a")


def spawn_evaluator(fixture: TaskCenterFixture) -> str:
    generator_id = apply_single_generator_plan(fixture)
    fixture.orchestrator.apply_generator_submission(
        GeneratorSubmission(
            attempt_id=fixture.attempt_id,
            task_id=generator_id,
            outcome="success",
            summary="done",
            payload={},
        )
    )
    return evaluator_task_id(fixture.attempt_id)
