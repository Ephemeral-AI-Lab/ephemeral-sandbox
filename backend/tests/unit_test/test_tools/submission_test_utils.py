"""Shared helpers for Phase 03 submission tool tests."""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
from typing import Any

from task_center.attempt.orchestrator import AttemptOrchestrator
from task_center.attempt.orchestrator_registry import (
    AttemptOrchestratorRegistry,
)
from task_center.attempt.deps import AgentLaunch, AttemptDeps
from task_center.iteration import OpenIterationCoordinatorRegistry
from task_center.iteration.state import IterationCreationReason
from task_center.submissions import GeneratorSubmission, PlannedGeneratorTask, PlannerSubmission
from task_center._core.primitives import evaluator_task_id, generator_task_id, planner_task_id
from tools._framework.core.context import ToolExecutionContextService
from tools._framework.core.runtime import ExecutionMetadata

from .test_submission._advisor_approval_fixtures import (
    build_advisor_approval_messages,
)


@dataclass
class TaskCenterFixture:
    runtime: AttemptDeps
    orchestrator: AttemptOrchestrator
    attempt_id: str
    request_id: str
    iteration_id: str


class FakeLauncher:
    def __init__(self) -> None:
        self.launches: list[AgentLaunch] = []

    def launch(self, launch: AgentLaunch) -> None:
        self.launches.append(launch)


def build_harness_fixture(
    *,
    workflow_store: Any,
    iteration_store: Any,
    attempt_store: Any,
    task_store: Any,
    composer: Any,
) -> TaskCenterFixture:
    request = workflow_store.insert(
        task_center_run_id="run1",
        requested_by_task_id="outer-task",
        goal="solve the task",
    )
    iteration = iteration_store.insert(
        workflow_id=request.id,
        sequence_no=1,
        creation_reason=IterationCreationReason.INITIAL,
        goal="solve the task",
        attempt_budget=2,
    )
    workflow_store.append_iteration_id(request.id, iteration.id)
    attempt = attempt_store.insert(iteration_id=iteration.id, attempt_sequence_no=1)
    iteration_store.append_attempt_id(iteration.id, attempt.id)

    launcher = FakeLauncher()
    registry = AttemptOrchestratorRegistry()
    runtime = AttemptDeps(
        workflow_store=workflow_store,
        iteration_store=iteration_store,
        attempt_store=attempt_store,
        task_store=task_store,
        agent_launcher=launcher,
        orchestrator_registry=registry,
        iteration_coordinators=OpenIterationCoordinatorRegistry(),
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
        iteration_id=iteration.id,
    )


def make_tool_context(
    fixture: TaskCenterFixture,
    task_id: str,
    *,
    messages: list[Any] | None = None,
    role: str | None = "executor",
    agent_type: str | None = None,
    advisor_approves: str | None = None,
) -> ToolExecutionContextService:
    """Build a tool execution context for a submission-tool test.

    ``advisor_approves`` accepts a terminal-tool name and prepends a synthetic
    ``ask_advisor`` approval pair to ``conversation_messages`` so the
    ``AdvisorApprovalPreHook`` lets the call through. Tests that explicitly want
    to exercise the unapproved path leave this kwarg unset.
    """
    base_messages: list[Any] = []
    if advisor_approves is not None:
        base_messages.extend(
            build_advisor_approval_messages(tool_name=advisor_approves)
        )
    if messages:
        base_messages.extend(messages)
    metadata = ExecutionMetadata(
        task_center_task_id=task_id,
        task_center_attempt_id=fixture.attempt_id,
        attempt_runtime=fixture.runtime,
        conversation_messages=base_messages,
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
            kind="completes",
            plan_spec="spec",
            evaluation_criteria=("criterion",),
            tasks=(
                PlannedGeneratorTask(
                    local_id="a",
                    agent_name=agent_name,
                    deps=(),
                    task_spec="do A",
                ),
            ),
            deferred_goal_for_next_iteration=None,
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
