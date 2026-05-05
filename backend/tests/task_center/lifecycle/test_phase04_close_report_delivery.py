"""Phase 04 close-report router tests."""

from __future__ import annotations

import pytest

from task_center.mission.close_report_delivery import (
    ComplexTaskCloseReportRouter,
)
from task_center.mission.mission import ComplexTaskCloseReport
from task_center.exceptions import GraphInvariantViolation
from task_center.attempt.orchestrator import HarnessGraphOrchestrator
from task_center.attempt.orchestrator_registry import (
    HarnessGraphOrchestratorRegistry,
)
from task_center.attempt.runtime import AgentLaunch, HarnessGraphRuntime
from task_center.episode.registry import SegmentManagerRegistry
from task_center.episode.episode import TaskSegmentCreationReason
from task_center.task import (
    HarnessTaskStatus,
    PlannedGeneratorTask,
    PlannerSubmission,
    generator_task_id,
)


class _FakeLauncher:
    def __init__(self) -> None:
        self.launches: list[AgentLaunch] = []

    def launch(self, launch: AgentLaunch) -> None:
        self.launches.append(launch)


def _build_runtime_with_open_graph(
    *,
    request_store,
    segment_store,
    graph_store,
    task_store,
    task_center_run_id: str,
    composer,
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
        composer=composer,
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
    request_store, segment_store, graph_store, task_store, task_center_run_id, composer
) -> None:
    runtime, parent_graph_id, parent_task_id = _build_runtime_with_open_graph(
        request_store=request_store,
        segment_store=segment_store,
        graph_store=graph_store,
        task_store=task_store,
        task_center_run_id=task_center_run_id,
        composer=composer,
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
    request_store, segment_store, graph_store, task_store, task_center_run_id, composer
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
        composer=composer,
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
    request_store, segment_store, graph_store, task_store, task_center_run_id, composer
) -> None:
    runtime, _, parent_task_id = _build_runtime_with_open_graph(
        request_store=request_store,
        segment_store=segment_store,
        graph_store=graph_store,
        task_store=task_store,
        task_center_run_id=task_center_run_id,
        composer=composer,
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
    request_store, segment_store, graph_store, task_store, task_center_run_id, composer
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
        composer=composer,
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
    request_store, segment_store, graph_store, task_store, task_center_run_id, composer
) -> None:
    runtime, _, parent_task_id = _build_runtime_with_open_graph(
        request_store=request_store,
        segment_store=segment_store,
        graph_store=graph_store,
        task_store=task_store,
        task_center_run_id=task_center_run_id,
        composer=composer,
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
    request_store, segment_store, graph_store, task_store, task_center_run_id, composer
) -> None:
    runtime, _, parent_task_id = _build_runtime_with_open_graph(
        request_store=request_store,
        segment_store=segment_store,
        graph_store=graph_store,
        task_store=task_store,
        task_center_run_id=task_center_run_id,
        composer=composer,
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


def test_router_routes_entry_mode_close_report_through_controller(
    request_store, segment_store, graph_store, task_store, task_center_run_id, composer
) -> None:
    """Entry-mode close-report dispatch.

    When the parent task has ``task_center_harness_graph_id=None``, the
    router must look up :attr:`HarnessGraphRuntime.entry_task_controller`
    instead of the orchestrator registry, and route the close report into
    the controller's ``apply_complex_task_close_report``.
    """
    from task_center.mission.handler import ComplexTaskRequestHandler
    from task_center.mission.mission import ComplexTaskRequestStatus
    from task_center.config import HarnessLifecycleConfig
    from task_center.entry_task_controller import EntryTaskController
    from task_center.episode.episode import TaskSegmentStatus
    from task_center.task import HarnessTaskRole

    # Seed entry-mode caller in WAITING_COMPLEX_TASK.
    entry_task_id = "entry-task"
    finished_runs: list = []

    def _finish(report):
        finished_runs.append(report)

    handler = ComplexTaskRequestHandler(
        request_store=request_store,
        segment_store=segment_store,
        graph_store=graph_store,
        manager_registry=SegmentManagerRegistry(),
        config=HarnessLifecycleConfig(),
        deliver_close_report=_finish,
    )
    entry_request = handler.create_mission_request(
        task_center_run_id=task_center_run_id,
        requested_by_task_id=entry_task_id,
        goal="entry goal",
    )
    entry_segment, _ = handler.create_initial_episode_with_manager(
        complex_task_request_id=entry_request.id
    )
    task_store.upsert_task(
        task_id=entry_task_id,
        task_center_run_id=task_center_run_id,
        role=HarnessTaskRole.GENERATOR.value,
        agent_name="entry_executor",
        task_input="entry goal",
        status=HarnessTaskStatus.WAITING_COMPLEX_TASK.value,
        summaries=[],
        needs=[],
        task_center_harness_graph_id=None,
        spawn_reason="entry_executor",
    )
    controller = EntryTaskController(
        task_id=entry_task_id,
        task_center_run_id=task_center_run_id,
        complex_task_request_id=entry_request.id,
        task_segment_id=entry_segment.id,
        task_store=task_store,
        segment_store=segment_store,
        request_handler=handler,
        manager_registry=handler._manager_registry,  # type: ignore[attr-defined]
    )
    runtime = HarnessGraphRuntime(
        request_store=request_store,
        segment_store=segment_store,
        graph_store=graph_store,
        task_store=task_store,
        agent_launcher=_FakeLauncher(),
        orchestrator_registry=HarnessGraphOrchestratorRegistry(),
        manager_registry=handler._manager_registry,  # type: ignore[attr-defined]
        composer=composer,
        entry_task_controller=controller,
    )

    router = ComplexTaskCloseReportRouter(runtime=runtime)
    result = router.deliver(
        ComplexTaskCloseReport(
            complex_task_request_id="delegated-x",
            requested_by_task_id=entry_task_id,
            outcome="success",
            final_segment_id="delegated-seg",
            final_harness_graph_id="delegated-graph",
        )
    )

    assert result.status == "delivered"
    assert result.parent_harness_graph_id is None
    entry_task = task_store.get_task(entry_task_id)
    fresh_segment = segment_store.get(entry_segment.id)
    fresh_request = request_store.get(entry_request.id)
    assert entry_task is not None
    assert entry_task["status"] == HarnessTaskStatus.DONE.value
    assert fresh_segment is not None
    assert fresh_segment.status == TaskSegmentStatus.SUCCEEDED
    assert fresh_request is not None
    assert fresh_request.status == ComplexTaskRequestStatus.SUCCEEDED
    # The close-report sink fires once; the run can finalize via that.
    assert len(finished_runs) == 1
    assert finished_runs[0].outcome == "success"
