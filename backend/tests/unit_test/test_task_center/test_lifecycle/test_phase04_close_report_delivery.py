"""Phase 04 close-report router tests."""

from __future__ import annotations

from typing import Literal

import pytest

from task_center.workflow.closure_report_router import (
    WorkflowClosureReportRouter,
)
from task_center.workflow.state import WorkflowClosureReport, WorkflowOriginKind
from task_center._core.primitives import TaskCenterInvariantViolation
from task_center.attempt.orchestrator import AttemptOrchestrator
from task_center.attempt.orchestrator_registry import (
    AttemptOrchestratorRegistry,
)
from task_center.attempt.deps import AgentLaunch, AttemptDeps
from task_center.iteration import OpenIterationCoordinatorRegistry
from task_center.iteration.state import IterationCreationReason
from task_center._core.task_state import TaskCenterTaskStatus
from task_center.submissions import PlannedGeneratorTask, PlannerSubmission
from task_center._core.primitives import generator_task_id


class _FakeLauncher:
    def __init__(self) -> None:
        self.launches: list[AgentLaunch] = []

    def launch(self, launch: AgentLaunch) -> None:
        self.launches.append(launch)


def _build_runtime_with_open_graph(
    *,
    workflow_store,
    iteration_store,
    attempt_store,
    task_store,
    task_center_run_id: str,
    composer,
):
    request = workflow_store.insert(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="parent-task",
        goal="outer",
    )
    iteration = iteration_store.insert(
        workflow_id=request.id,
        sequence_no=1,
        creation_reason=IterationCreationReason.INITIAL,
        goal="outer",
        attempt_budget=2,
    )
    workflow_store.append_iteration_id(request.id, iteration.id)
    attempt = attempt_store.insert(iteration_id=iteration.id, attempt_sequence_no=1)
    iteration_store.append_attempt_id(iteration.id, attempt.id)
    registry = AttemptOrchestratorRegistry()
    runtime = AttemptDeps(
        workflow_store=workflow_store,
        iteration_store=iteration_store,
        attempt_store=attempt_store,
        task_store=task_store,
        agent_launcher=_FakeLauncher(),
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
    orchestrator.start()
    orchestrator.apply_plan_submission(
        PlannerSubmission(
            attempt_id=attempt.id,
            planner_task_id=f"{attempt.id}:planner",
            kind="completes",
            plan_spec="spec",
            evaluation_criteria=("c",),
            tasks=(
                PlannedGeneratorTask(
                    local_id="a",
                    agent_name="executor",
                    deps=(),
                    task_spec="do",
                ),
            ),
            deferred_goal_for_next_iteration=None,
            summary="plan",
        )
    )
    parent_task_id = generator_task_id(attempt.id, "a")
    return runtime, attempt.id, parent_task_id


def _set_parent_waiting(task_store, parent_task_id: str) -> None:
    task_store.set_task_status(
        parent_task_id,
        status=TaskCenterTaskStatus.WAITING_WORKFLOW.value,
    )


def _task_report(
    *,
    task_center_run_id: str,
    requested_by_task_id: str,
    outcome: Literal["success", "failed"] = "success",
) -> WorkflowClosureReport:
    return WorkflowClosureReport(
        workflow_id="delegated-1",
        task_center_run_id=task_center_run_id,
        origin_kind=WorkflowOriginKind.TASK,
        requested_by_task_id=requested_by_task_id,
        outcome=outcome,
        final_iteration_id="seg-1",
        final_attempt_id="attempt-1",
    )


def test_router_delivers_success_to_waiting_parent(
    workflow_store, iteration_store, attempt_store, task_store, task_center_run_id, composer
) -> None:
    runtime, parent_attempt_id, parent_task_id = _build_runtime_with_open_graph(
        workflow_store=workflow_store,
        iteration_store=iteration_store,
        attempt_store=attempt_store,
        task_store=task_store,
        task_center_run_id=task_center_run_id,
        composer=composer,
    )
    _set_parent_waiting(task_store, parent_task_id)
    router = WorkflowClosureReportRouter(runtime=runtime)

    result = router.deliver(
        _task_report(
            task_center_run_id=task_center_run_id,
            requested_by_task_id=parent_task_id,
        )
    )

    assert result.status == "delivered"
    assert result.parent_attempt_id == parent_attempt_id
    parent_task = task_store.get_task(parent_task_id)
    assert parent_task is not None
    assert parent_task["status"] == TaskCenterTaskStatus.DONE.value


def test_router_delivers_failure_marks_parent_failed_and_blocks_dependents(
    workflow_store, iteration_store, attempt_store, task_store, task_center_run_id, composer
) -> None:
    request = workflow_store.insert(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="parent-task",
        goal="outer",
    )
    iteration = iteration_store.insert(
        workflow_id=request.id,
        sequence_no=1,
        creation_reason=IterationCreationReason.INITIAL,
        goal="outer",
        attempt_budget=2,
    )
    workflow_store.append_iteration_id(request.id, iteration.id)
    attempt = attempt_store.insert(iteration_id=iteration.id, attempt_sequence_no=1)
    iteration_store.append_attempt_id(iteration.id, attempt.id)
    registry = AttemptOrchestratorRegistry()
    runtime = AttemptDeps(
        workflow_store=workflow_store,
        iteration_store=iteration_store,
        attempt_store=attempt_store,
        task_store=task_store,
        agent_launcher=_FakeLauncher(),
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
    orchestrator.start()
    orchestrator.apply_plan_submission(
        PlannerSubmission(
            attempt_id=attempt.id,
            planner_task_id=f"{attempt.id}:planner",
            kind="completes",
            plan_spec="spec",
            evaluation_criteria=("c",),
            tasks=(
                PlannedGeneratorTask("a", "executor", (), "do A"),
                PlannedGeneratorTask("b", "executor", ("a",), "do B"),
            ),
            deferred_goal_for_next_iteration=None,
            summary="plan",
        )
    )
    parent_task_id = generator_task_id(attempt.id, "a")
    dependent_id = generator_task_id(attempt.id, "b")
    _set_parent_waiting(task_store, parent_task_id)
    router = WorkflowClosureReportRouter(runtime=runtime)

    result = router.deliver(
        _task_report(
            task_center_run_id=task_center_run_id,
            requested_by_task_id=parent_task_id,
            outcome="failed",
        )
    )

    assert result.status == "delivered"
    parent_task = task_store.get_task(parent_task_id)
    dependent = task_store.get_task(dependent_id)
    assert parent_task is not None
    assert parent_task["status"] == TaskCenterTaskStatus.FAILED.value
    assert dependent is not None
    assert dependent["status"] == TaskCenterTaskStatus.PENDING.value


def test_router_treats_done_parent_as_already_delivered(
    workflow_store, iteration_store, attempt_store, task_store, task_center_run_id, composer
) -> None:
    runtime, _, parent_task_id = _build_runtime_with_open_graph(
        workflow_store=workflow_store,
        iteration_store=iteration_store,
        attempt_store=attempt_store,
        task_store=task_store,
        task_center_run_id=task_center_run_id,
        composer=composer,
    )
    task_store.set_task_status(
        parent_task_id, status=TaskCenterTaskStatus.DONE.value
    )
    router = WorkflowClosureReportRouter(runtime=runtime)

    result = router.deliver(
        _task_report(
            task_center_run_id=task_center_run_id,
            requested_by_task_id=parent_task_id,
        )
    )

    assert result.status == "already_delivered"


def test_router_raises_when_parent_orchestrator_missing(
    workflow_store, iteration_store, attempt_store, task_store, task_center_run_id, composer
) -> None:
    """No-restart invariant: while a parent task is in WAITING_WORKFLOW
    its orchestrator must remain registered. A missing orchestrator at
    delivery time is a hard ``TaskCenterInvariantViolation``."""
    runtime, parent_attempt_id, parent_task_id = _build_runtime_with_open_graph(
        workflow_store=workflow_store,
        iteration_store=iteration_store,
        attempt_store=attempt_store,
        task_store=task_store,
        task_center_run_id=task_center_run_id,
        composer=composer,
    )
    _set_parent_waiting(task_store, parent_task_id)
    runtime.orchestrator_registry.deregister(parent_attempt_id)
    router = WorkflowClosureReportRouter(runtime=runtime)

    with pytest.raises(TaskCenterInvariantViolation):
        router.deliver(
            _task_report(
                task_center_run_id=task_center_run_id,
                requested_by_task_id=parent_task_id,
            )
        )

    parent_task = task_store.get_task(parent_task_id)
    assert parent_task is not None
    assert parent_task["status"] == TaskCenterTaskStatus.WAITING_WORKFLOW.value


def test_router_rejects_running_parent(
    workflow_store, iteration_store, attempt_store, task_store, task_center_run_id, composer
) -> None:
    runtime, _, parent_task_id = _build_runtime_with_open_graph(
        workflow_store=workflow_store,
        iteration_store=iteration_store,
        attempt_store=attempt_store,
        task_store=task_store,
        task_center_run_id=task_center_run_id,
        composer=composer,
    )
    # Parent is RUNNING (not waiting) — illegal report state.
    router = WorkflowClosureReportRouter(runtime=runtime)

    with pytest.raises(TaskCenterInvariantViolation):
        router.deliver(
            _task_report(
                task_center_run_id=task_center_run_id,
                requested_by_task_id=parent_task_id,
            )
        )


def test_apply_closure_report_is_idempotent_on_second_delivery(
    workflow_store, iteration_store, attempt_store, task_store, task_center_run_id, composer
) -> None:
    runtime, _, parent_task_id = _build_runtime_with_open_graph(
        workflow_store=workflow_store,
        iteration_store=iteration_store,
        attempt_store=attempt_store,
        task_store=task_store,
        task_center_run_id=task_center_run_id,
        composer=composer,
    )
    _set_parent_waiting(task_store, parent_task_id)
    parent_task_before = task_store.get_task(parent_task_id)
    assert parent_task_before is not None
    summary_count_before = len(parent_task_before["summaries"])

    report = _task_report(
        task_center_run_id=task_center_run_id,
        requested_by_task_id=parent_task_id,
    )
    # Find the orchestrator and apply the close report twice. Second call
    # must be silently idempotent (CAS miss).
    parent_attempt_id = parent_task_before["task_center_attempt_id"]
    orchestrator = runtime.orchestrator_registry.get_or_raise(parent_attempt_id)
    orchestrator.apply_workflow_closure_report(report)
    orchestrator.apply_workflow_closure_report(report)

    parent_task = task_store.get_task(parent_task_id)
    assert parent_task is not None
    assert parent_task["status"] == TaskCenterTaskStatus.DONE.value
    # Exactly one new summary appended.
    assert len(parent_task["summaries"]) == summary_count_before + 1


def test_router_finishes_entry_origin_goal_run(
    workflow_store, iteration_store, attempt_store, task_store, task_center_run_id, composer
) -> None:
    runtime = AttemptDeps(
        workflow_store=workflow_store,
        iteration_store=iteration_store,
        attempt_store=attempt_store,
        task_store=task_store,
        agent_launcher=_FakeLauncher(),
        orchestrator_registry=AttemptOrchestratorRegistry(),
        iteration_coordinators=OpenIterationCoordinatorRegistry(),
        composer=composer,
    )

    router = WorkflowClosureReportRouter(runtime=runtime)
    result = router.deliver(
        WorkflowClosureReport(
            workflow_id="delegated-x",
            task_center_run_id=task_center_run_id,
            origin_kind=WorkflowOriginKind.ENTRY,
            requested_by_task_id=None,
            outcome="success",
            final_iteration_id="delegated-seg",
            final_attempt_id="delegated-attempt",
        )
    )

    assert result.status == "delivered"
    assert result.parent_attempt_id is None
    run = task_store.get_run(task_center_run_id)
    assert run is not None
    assert run["status"] == "done"
