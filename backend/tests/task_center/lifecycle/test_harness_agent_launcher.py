"""Production harness agent launcher tests."""

from __future__ import annotations

import pytest

from agents.registry import get_definition, register_definition, unregister_definition
from agents.types import AgentDefinition
from engine.runtime.lifecycle import EphemeralRunResult
from server.app_factory import RuntimeConfig
from task_center.harness_graph.graph import HarnessGraphFailReason, HarnessGraphStatus
from task_center.harness_graph.launcher import EphemeralHarnessAgentLauncher
from task_center.harness_graph.orchestrator import HarnessGraphOrchestrator
from task_center.harness_graph.orchestrator_registry import (
    HarnessGraphOrchestratorRegistry,
)
from task_center.harness_graph.runtime import HarnessGraphRuntime
from task_center.segment.registry import SegmentManagerRegistry
from task_center.segment.segment import TaskSegmentCreationReason
from task_center.task import HarnessTaskStatus, planner_task_id


@pytest.mark.asyncio
async def test_launcher_passes_metadata_and_routes_planner_exhaustion(
    request_store,
    segment_store,
    graph_store,
    task_store,
    task_center_run_id,
    tmp_path,
) -> None:
    previous = get_definition("planner")
    register_definition(
        AgentDefinition(
            name="planner",
            description="test planner",
            role="planner",
            terminals=["submit_full_plan", "submit_partial_plan"],
        )
    )
    captured: list[dict[str, object]] = []

    async def fake_runner(*args, **kwargs):
        del args
        captured.append(kwargs)
        return EphemeralRunResult(
            status="completed",
            error=None,
            terminal_result=None,
            agent_name="planner",
            event_count=0,
        )

    runtime_ref: HarnessGraphRuntime | None = None
    launcher = EphemeralHarnessAgentLauncher(
        config=RuntimeConfig(cwd=str(tmp_path)),
        runtime=lambda: runtime_ref,
        runner=fake_runner,
    )
    runtime = HarnessGraphRuntime(
        request_store=request_store,
        segment_store=segment_store,
        graph_store=graph_store,
        task_store=task_store,
        agent_launcher=launcher,
        orchestrator_registry=HarnessGraphOrchestratorRegistry(),
        manager_registry=SegmentManagerRegistry(),
    )
    runtime_ref = runtime

    request = request_store.insert(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="entry",
        goal="plan this",
    )
    segment = segment_store.insert(
        complex_task_request_id=request.id,
        sequence_no=1,
        creation_reason=TaskSegmentCreationReason.INITIAL,
        goal="plan this",
        attempt_budget=1,
    )
    request_store.append_segment_id(request.id, segment.id)
    graph = graph_store.insert(task_segment_id=segment.id, graph_sequence_no=1)
    segment_store.append_graph_id(segment.id, graph.id)

    closed: list[str] = []
    orchestrator = HarnessGraphOrchestrator(
        harness_graph=graph,
        on_graph_closed=closed.append,
        runtime=runtime,
    )

    try:
        orchestrator.start()
        await launcher.wait_for_idle()
    finally:
        if previous is None:
            unregister_definition("planner")
        else:
            register_definition(previous)

    assert len(captured) == 1
    metadata = captured[0]["extra_tool_metadata"]
    assert metadata.task_center_task_id == planner_task_id(graph.id)
    assert metadata.task_center_harness_graph_id == graph.id
    assert metadata.harness_graph_runtime is runtime

    planner_task = task_store.get_task(planner_task_id(graph.id))
    latest_graph = graph_store.get(graph.id)
    assert planner_task is not None
    assert planner_task["status"] == HarnessTaskStatus.FAILED.value
    assert latest_graph is not None
    assert latest_graph.status == HarnessGraphStatus.FAILED
    assert latest_graph.fail_reason == HarnessGraphFailReason.PLANNER_FAILED
    assert closed == [graph.id]
