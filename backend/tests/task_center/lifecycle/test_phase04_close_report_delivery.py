"""Phase 04 close-report router tests."""

from __future__ import annotations

import pytest

from task_center.complex_task.close_report_delivery import (
    ComplexTaskCloseReportRouter,
)
from task_center.complex_task.request import ComplexTaskCloseReport
from task_center.exceptions import GraphInvariantViolation
from task_center.harness_graph.orchestrator import HarnessGraphOrchestrator
from task_center.harness_graph.orchestrator_registry import (
    HarnessGraphOrchestratorRegistry,
)
from task_center.harness_graph.runtime import HarnessAgentLaunch, HarnessGraphRuntime
from task_center.segment.registry import SegmentManagerRegistry
from task_center.segment.segment import TaskSegmentCreationReason
from task_center.task import (
    HarnessTaskStatus,
    PlannedGeneratorTask,
    PlannerSubmission,
    generator_task_id,
)


class _FakeLauncher:
    def __init__(self) -> None:
        self.launches: list[HarnessAgentLaunch] = []

    def launch(self, launch: HarnessAgentLaunch) -> None:
        self.launches.append(launch)


def _build_runtime_with_open_graph(
    *, request_store, segment_store, graph_store, task_store, task_center_run_id: str
):
    request = request_store.insert(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="root",
        goal="outer",
    )
    segment = segment_store.insert(
        complex_task_request_id=request.id,
        sequence_no=1,
        creation_reason=TaskSegmentCreationReason.INITIAL,
        goal="outer",
        attempt_budget=2,
    )
    request_store.append_segment_id(request.id, segment.id)
    graph = graph_store.insert(task_segment_id=segment.id, graph_sequence_no=1)
    segment_store.append_graph_id(segment.id, graph.id)
    registry = HarnessGraphOrchestratorRegistry()
    runtime = HarnessGraphRuntime(
        request_store=request_store,
        segment_store=segment_store,
        graph_store=graph_store,
        task_store=task_store,
        agent_launcher=_FakeLauncher(),
        orchestrator_registry=registry,
        manager_registry=SegmentManagerRegistry(),
    )
    orchestrator = HarnessGraphOrchestrator(
        harness_graph=graph,
        on_graph_closed=lambda graph_id: None,
        runtime=runtime,
    )
    registry.register(orchestrator)
    orchestrator.start()
    orchestrator.apply_plan_submission(
        PlannerSubmission(
            graph_id=graph.id,
            planner_task_id=f"{graph.id}:planner",
            kind="full",
            task_specification="spec",
            evaluation_criteria=("c",),
            tasks=(
                PlannedGeneratorTask(
                    local_id="a",
                    agent_name="executor",
                    deps=(),
                    task_spec="do",
                ),
            ),
            continuation_goal=None,
            summary="plan",
        )
    )
    parent_task_id = generator_task_id(graph.id, "a")
    return runtime, graph.id, parent_task_id


def _set_parent_waiting(task_store, parent_task_id: str) -> None:
    task_store.set_task_status(
        parent_task_id,
        status=HarnessTaskStatus.WAITING_COMPLEX_TASK.value,
    )


def test_router_delivers_success_to_waiting_parent(
    request_store, segment_store, graph_store, task_store, task_center_run_id
) -> None:
    runtime, parent_graph_id, parent_task_id = _build_runtime_with_open_graph(
        request_store=request_store,
        segment_store=segment_store,
        graph_store=graph_store,
        task_store=task_store,
        task_center_run_id=task_center_run_id,
    )
    _set_parent_waiting(task_store, parent_task_id)
    router = ComplexTaskCloseReportRouter(runtime=runtime)

    result = router.deliver(
        ComplexTaskCloseReport(
            complex_task_request_id="delegated-1",
            requested_by_task_id=parent_task_id,
            outcome="success",
            final_segment_id="seg-1",
            final_harness_graph_id="graph-1",
        )
    )

    assert result.status == "delivered"
    assert result.parent_harness_graph_id == parent_graph_id
    parent_task = task_store.get_task(parent_task_id)
    assert parent_task is not None
    assert parent_task["status"] == HarnessTaskStatus.DONE.value


def test_router_delivers_failure_marks_parent_failed_and_blocks_dependents(
    request_store, segment_store, graph_store, task_store, task_center_run_id
) -> None:
    request = request_store.insert(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="root",
        goal="outer",
    )
    segment = segment_store.insert(
        complex_task_request_id=request.id,
        sequence_no=1,
        creation_reason=TaskSegmentCreationReason.INITIAL,
        goal="outer",
        attempt_budget=2,
    )
    request_store.append_segment_id(request.id, segment.id)
    graph = graph_store.insert(task_segment_id=segment.id, graph_sequence_no=1)
    segment_store.append_graph_id(segment.id, graph.id)
    registry = HarnessGraphOrchestratorRegistry()
    runtime = HarnessGraphRuntime(
        request_store=request_store,
        segment_store=segment_store,
        graph_store=graph_store,
        task_store=task_store,
        agent_launcher=_FakeLauncher(),
        orchestrator_registry=registry,
        manager_registry=SegmentManagerRegistry(),
    )
    orchestrator = HarnessGraphOrchestrator(
        harness_graph=graph,
        on_graph_closed=lambda graph_id: None,
        runtime=runtime,
    )
    registry.register(orchestrator)
    orchestrator.start()
    orchestrator.apply_plan_submission(
        PlannerSubmission(
            graph_id=graph.id,
            planner_task_id=f"{graph.id}:planner",
            kind="full",
            task_specification="spec",
            evaluation_criteria=("c",),
            tasks=(
                PlannedGeneratorTask("a", "executor", (), "do A"),
                PlannedGeneratorTask("b", "executor", ("a",), "do B"),
            ),
            continuation_goal=None,
            summary="plan",
        )
    )
    parent_task_id = generator_task_id(graph.id, "a")
    dependent_id = generator_task_id(graph.id, "b")
    _set_parent_waiting(task_store, parent_task_id)
    router = ComplexTaskCloseReportRouter(runtime=runtime)

    result = router.deliver(
        ComplexTaskCloseReport(
            complex_task_request_id="delegated-1",
            requested_by_task_id=parent_task_id,
            outcome="failed",
            final_segment_id="seg-1",
            final_harness_graph_id="graph-1",
        )
    )

    assert result.status == "delivered"
    parent_task = task_store.get_task(parent_task_id)
    dependent = task_store.get_task(dependent_id)
    assert parent_task is not None
    assert parent_task["status"] == HarnessTaskStatus.FAILED.value
    assert dependent is not None
    assert dependent["status"] == HarnessTaskStatus.BLOCKED.value


def test_router_treats_done_parent_as_already_delivered(
    request_store, segment_store, graph_store, task_store, task_center_run_id
) -> None:
    runtime, _, parent_task_id = _build_runtime_with_open_graph(
        request_store=request_store,
        segment_store=segment_store,
        graph_store=graph_store,
        task_store=task_store,
        task_center_run_id=task_center_run_id,
    )
    task_store.set_task_status(
        parent_task_id, status=HarnessTaskStatus.DONE.value
    )
    router = ComplexTaskCloseReportRouter(runtime=runtime)

    result = router.deliver(
        ComplexTaskCloseReport(
            complex_task_request_id="delegated-1",
            requested_by_task_id=parent_task_id,
            outcome="success",
            final_segment_id="seg-1",
            final_harness_graph_id="graph-1",
        )
    )

    assert result.status == "already_delivered"


def test_router_raises_when_parent_orchestrator_missing(
    request_store, segment_store, graph_store, task_store, task_center_run_id
) -> None:
    """No-restart invariant: while a parent task is in WAITING_COMPLEX_TASK
    its orchestrator must remain registered. A missing orchestrator at
    delivery time is a hard ``GraphInvariantViolation``."""
    runtime, parent_graph_id, parent_task_id = _build_runtime_with_open_graph(
        request_store=request_store,
        segment_store=segment_store,
        graph_store=graph_store,
        task_store=task_store,
        task_center_run_id=task_center_run_id,
    )
    _set_parent_waiting(task_store, parent_task_id)
    runtime.orchestrator_registry.deregister(parent_graph_id)
    router = ComplexTaskCloseReportRouter(runtime=runtime)

    with pytest.raises(GraphInvariantViolation):
        router.deliver(
            ComplexTaskCloseReport(
                complex_task_request_id="delegated-1",
                requested_by_task_id=parent_task_id,
                outcome="success",
                final_segment_id="seg-1",
                final_harness_graph_id="graph-1",
            )
        )

    parent_task = task_store.get_task(parent_task_id)
    assert parent_task is not None
    assert parent_task["status"] == HarnessTaskStatus.WAITING_COMPLEX_TASK.value


def test_router_rejects_running_parent(
    request_store, segment_store, graph_store, task_store, task_center_run_id
) -> None:
    runtime, _, parent_task_id = _build_runtime_with_open_graph(
        request_store=request_store,
        segment_store=segment_store,
        graph_store=graph_store,
        task_store=task_store,
        task_center_run_id=task_center_run_id,
    )
    # Parent is RUNNING (not waiting) — illegal report state.
    router = ComplexTaskCloseReportRouter(runtime=runtime)

    with pytest.raises(GraphInvariantViolation):
        router.deliver(
            ComplexTaskCloseReport(
                complex_task_request_id="delegated-1",
                requested_by_task_id=parent_task_id,
                outcome="success",
                final_segment_id="seg-1",
                final_harness_graph_id="graph-1",
            )
        )


def test_apply_close_report_is_idempotent_on_second_delivery(
    request_store, segment_store, graph_store, task_store, task_center_run_id
) -> None:
    runtime, _, parent_task_id = _build_runtime_with_open_graph(
        request_store=request_store,
        segment_store=segment_store,
        graph_store=graph_store,
        task_store=task_store,
        task_center_run_id=task_center_run_id,
    )
    _set_parent_waiting(task_store, parent_task_id)
    parent_task_before = task_store.get_task(parent_task_id)
    assert parent_task_before is not None
    summary_count_before = len(parent_task_before["summaries"])

    report = ComplexTaskCloseReport(
        complex_task_request_id="delegated-1",
        requested_by_task_id=parent_task_id,
        outcome="success",
        final_segment_id="seg-1",
        final_harness_graph_id="graph-1",
    )
    # Find the orchestrator and apply the close report twice. Second call
    # must be silently idempotent (CAS miss).
    parent_graph_id = parent_task_before["task_center_harness_graph_id"]
    orchestrator = runtime.orchestrator_registry.get_or_raise(parent_graph_id)
    orchestrator.apply_complex_task_close_report(report)
    orchestrator.apply_complex_task_close_report(report)

    parent_task = task_store.get_task(parent_task_id)
    assert parent_task is not None
    assert parent_task["status"] == HarnessTaskStatus.DONE.value
    # Exactly one new summary appended.
    assert len(parent_task["summaries"]) == summary_count_before + 1
