"""Shared helpers for Phase 03 submission tool tests."""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
from typing import Any

from task_center.attempt.orchestrator import HarnessGraphOrchestrator
from task_center.attempt.orchestrator_registry import (
    HarnessGraphOrchestratorRegistry,
)
from task_center.attempt.runtime import AgentLaunch, HarnessGraphRuntime
from task_center.episode.registry import SegmentManagerRegistry
from task_center.episode.episode import TaskSegmentCreationReason
from task_center.task import (
    GeneratorSubmission,
    PlannedGeneratorTask,
    PlannerSubmission,
    evaluator_task_id,
    generator_task_id,
    planner_task_id,
)
from tools.core.context import ToolExecutionContextService
from tools.core.runtime import ExecutionMetadata


@dataclass
class HarnessFixture:
    runtime: HarnessGraphRuntime
    orchestrator: HarnessGraphOrchestrator
    graph_id: str
    request_id: str
    segment_id: str


class FakeLauncher:
    def __init__(self) -> None:
        self.launches: list[AgentLaunch] = []

    def launch(self, launch: AgentLaunch) -> None:
        self.launches.append(launch)


def build_harness_fixture(
    *,
    request_store: Any,
    segment_store: Any,
    graph_store: Any,
    task_store: Any,
    composer: Any,
) -> HarnessFixture:
    request = request_store.insert(
        task_center_run_id="run1",
        requested_by_task_id="outer-task",
        goal="solve the task",
    )
    segment = segment_store.insert(
        complex_task_request_id=request.id,
        sequence_no=1,
        creation_reason=TaskSegmentCreationReason.INITIAL,
        goal="solve the task",
        attempt_budget=2,
    )
    request_store.append_segment_id(request.id, segment.id)
    graph = graph_store.insert(task_segment_id=segment.id, graph_sequence_no=1)
    segment_store.append_graph_id(segment.id, graph.id)

    launcher = FakeLauncher()
    registry = HarnessGraphOrchestratorRegistry()
    runtime = HarnessGraphRuntime(
        request_store=request_store,
        segment_store=segment_store,
        graph_store=graph_store,
        task_store=task_store,
        agent_launcher=launcher,
        orchestrator_registry=registry,
        manager_registry=SegmentManagerRegistry(),
        composer=composer,
    )
    orchestrator = HarnessGraphOrchestrator(
        harness_graph=graph,
        on_graph_closed=lambda graph_id: None,
        runtime=runtime,
    )
    registry.register(orchestrator)
    return HarnessFixture(
        runtime=runtime,
        orchestrator=orchestrator,
        graph_id=graph.id,
        request_id=request.id,
        segment_id=segment.id,
    )


def make_tool_context(
    fixture: HarnessFixture,
    task_id: str,
    *,
    messages: list[Any] | None = None,
    role: str | None = "executor",
    agent_type: str | None = None,
) -> ToolExecutionContextService:
    metadata = ExecutionMetadata(
        task_center_task_id=task_id,
        task_center_harness_graph_id=fixture.graph_id,
        harness_graph_runtime=fixture.runtime,
        conversation_messages=list(messages or []),
    )
    if role is not None:
        metadata["role"] = role
    if agent_type is not None:
        metadata["agent_type"] = agent_type
    return ToolExecutionContextService(cwd=Path("/tmp"), services=metadata)


def start_planner(fixture: HarnessFixture) -> str:
    fixture.orchestrator.start()
    return planner_task_id(fixture.graph_id)


def apply_single_generator_plan(fixture: HarnessFixture, *, agent_name: str = "executor") -> str:
    planner_id = start_planner(fixture)
    fixture.orchestrator.apply_plan_submission(
        PlannerSubmission(
            graph_id=fixture.graph_id,
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
    return generator_task_id(fixture.graph_id, "a")


def spawn_evaluator(fixture: HarnessFixture) -> str:
    generator_id = apply_single_generator_plan(fixture)
    fixture.orchestrator.apply_generator_submission(
        GeneratorSubmission(
            graph_id=fixture.graph_id,
            task_id=generator_id,
            outcome="success",
            summary="done",
            payload={},
        )
    )
    return evaluator_task_id(fixture.graph_id)
