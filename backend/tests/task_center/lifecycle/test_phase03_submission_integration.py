"""Phase 03 submission tool integration smoke tests."""

from __future__ import annotations

import pytest

from task_center.attempt import HarnessGraphStatus
from task_center.attempt.orchestrator import HarnessGraphOrchestrator
from task_center.attempt.orchestrator_registry import (
    HarnessGraphOrchestratorRegistry,
)
from task_center.attempt.runtime import AgentLaunch, HarnessGraphRuntime
from task_center.episode.registry import SegmentManagerRegistry
from task_center.episode.episode import TaskSegmentCreationReason
from task_center.task import evaluator_task_id, generator_task_id, planner_task_id
from tools.core.context import ToolExecutionContextService
from tools.core.runtime import ExecutionMetadata
from tools.core.tool_execution import execute_tool_once
from tools.submission.main_agent.evaluator import submit_evaluation_success
from tools.submission.main_agent.generator.executor import submit_execution_success
from tools.submission.main_agent.planner import submit_full_plan

pytestmark = pytest.mark.asyncio


class _FakeLauncher:
    def __init__(self) -> None:
        self.launches: list[AgentLaunch] = []

    def launch(self, launch: AgentLaunch) -> None:
        self.launches.append(launch)


async def _noop_emit(event) -> None:
    del event


def _tool_context(
    runtime: HarnessGraphRuntime,
    graph_id: str,
    task_id: str,
    *,
    role: str = "executor",
):
    metadata = ExecutionMetadata(
        task_center_task_id=task_id,
        task_center_harness_graph_id=graph_id,
        harness_graph_runtime=runtime,
    )
    metadata["role"] = role
    return ToolExecutionContextService(cwd="/tmp", services=metadata)


def _build_runtime(request_store, segment_store, graph_store, task_store, *, composer):
    request = request_store.insert(
        task_center_run_id="run1",
        requested_by_task_id="outer-task",
        goal="solve task",
    )
    segment = segment_store.insert(
        complex_task_request_id=request.id,
        sequence_no=1,
        creation_reason=TaskSegmentCreationReason.INITIAL,
        goal="solve task",
        attempt_budget=2,
    )
    request_store.append_segment_id(request.id, segment.id)
    graph = graph_store.insert(task_segment_id=segment.id, graph_sequence_no=1)
    segment_store.append_graph_id(segment.id, graph.id)
    launcher = _FakeLauncher()
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
    return runtime, orchestrator, graph.id


async def test_phase03_full_plan_through_evaluator_success(
    request_store, segment_store, graph_store, task_store, composer
) -> None:
    runtime, orchestrator, graph_id = _build_runtime(
        request_store,
        segment_store,
        graph_store,
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
        _tool_context(runtime, graph_id, planner_task_id(graph_id)),
        emit=_noop_emit,
    )
    generator_result = await execute_tool_once(
        submit_execution_success,
        {"summary": "done", "artifacts": []},
        _tool_context(runtime, graph_id, generator_task_id(graph_id, "a")),
        emit=_noop_emit,
    )
    evaluator_result = await execute_tool_once(
        submit_evaluation_success,
        {"summary": "passed", "passed_criteria": ["generator passed"]},
        _tool_context(runtime, graph_id, evaluator_task_id(graph_id)),
        emit=_noop_emit,
    )

    graph = graph_store.get(graph_id)
    assert not planner_result.is_error
    assert not generator_result.is_error
    assert not evaluator_result.is_error
    assert graph is not None
    assert graph.status == HarnessGraphStatus.PASSED
