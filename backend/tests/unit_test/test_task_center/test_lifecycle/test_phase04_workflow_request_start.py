"""Phase 04 goal request starter tests.

Covers happy path, startup failure rollback, and duplicate-open-request gating.
"""

from __future__ import annotations

import pytest

from task_center.workflow.starter import (
    WorkflowStarter,
    StartedWorkflow,
)
from task_center.workflow.state import WorkflowOrigin, WorkflowOriginKind, WorkflowStatus
from task_center._core.primitives import TaskCenterInvariantViolation
from task_center.attempt.orchestrator_registry import (
    AttemptOrchestratorRegistry,
)
from task_center.attempt.deps import AgentLaunch, AttemptDeps
from task_center.attempt import (
    AttemptFailReason,
    AttemptStatus,
)
from task_center.iteration import OpenIterationCoordinatorRegistry
from task_center.iteration.state import (
    IterationCreationReason,
    IterationStatus,
)
from task_center._core.task_state import TaskCenterTaskRole, TaskCenterTaskStatus
from task_center._core.primitives import planner_task_id


class _FakeLauncher:
    def __init__(self) -> None:
        self.launches: list[AgentLaunch] = []

    def launch(self, launch: AgentLaunch) -> None:
        self.launches.append(launch)


class _FailingLauncher:
    def launch(self, launch: AgentLaunch) -> None:
        del launch
        raise RuntimeError("delegated planner launch boom")


def _build_runtime(
    workflow_store, iteration_store, attempt_store, task_store, *, composer, launcher=None
) -> AttemptDeps:
    launcher = launcher or _FakeLauncher()
    registry = AttemptOrchestratorRegistry()
    return AttemptDeps(
        workflow_store=workflow_store,
        iteration_store=iteration_store,
        attempt_store=attempt_store,
        task_store=task_store,
        agent_launcher=launcher,
        orchestrator_registry=registry,
        iteration_coordinators=OpenIterationCoordinatorRegistry(),
        composer=composer,
    )


def _seed_outer_generator_task(
    *,
    task_store,
    workflow_store,
    iteration_store,
    attempt_store,
    task_center_run_id: str,
) -> tuple[str, str]:
    """Seed an outer generator task whose attempt is currently RUNNING."""
    outer_request = workflow_store.insert(
        task_center_run_id=task_center_run_id,
        requested_by_task_id="parent-task",
        goal="outer goal",
    )
    outer_segment = iteration_store.insert(
        workflow_id=outer_request.id,
        sequence_no=1,
        creation_reason=IterationCreationReason.INITIAL,
        goal="outer goal",
        attempt_budget=2,
    )
    workflow_store.append_iteration_id(outer_request.id, outer_segment.id)
    outer_attempt = attempt_store.insert(
        iteration_id=outer_segment.id, attempt_sequence_no=1
    )
    iteration_store.append_attempt_id(outer_segment.id, outer_attempt.id)

    parent_task_id = "outer-generator-task"
    task_store.upsert_task(
        task_id=parent_task_id,
        task_center_run_id=task_center_run_id,
        role=TaskCenterTaskRole.GENERATOR.value,
        agent_name="executor",
        context_message="execute the outer task",
        status=TaskCenterTaskStatus.RUNNING.value,
        summaries=[],
        needs=[],
        task_center_attempt_id=outer_attempt.id,
        spawn_reason="attempt_generator",
    )
    return parent_task_id, outer_attempt.id


def test_workflow_start_creates_request_segment_graph_and_marks_parent_waiting(
    workflow_store, iteration_store, attempt_store, task_store, task_center_run_id, composer
) -> None:
    runtime = _build_runtime(
        workflow_store, iteration_store, attempt_store, task_store, composer=composer
    )
    parent_task_id, parent_attempt_id = _seed_outer_generator_task(
        task_store=task_store,
        workflow_store=workflow_store,
        iteration_store=iteration_store,
        attempt_store=attempt_store,
        task_center_run_id=task_center_run_id,
    )
    coordinator = WorkflowStarter(runtime=runtime)

    result: StartedWorkflow = coordinator.start(
        prompt="solve delegated task",
        origin=WorkflowOrigin.task(task_id=parent_task_id),
    )

    delegated_request = workflow_store.get(result.workflow_id)
    initial_iteration = iteration_store.get(result.initial_iteration_id)
    initial_graph = attempt_store.get(result.initial_attempt_id)
    parent_task = task_store.get_task(parent_task_id)

    assert delegated_request is not None
    assert delegated_request.status == WorkflowStatus.OPEN
    assert delegated_request.origin_kind == WorkflowOriginKind.TASK
    assert delegated_request.requested_by_task_id == parent_task_id
    assert delegated_request.goal == "solve delegated task"
    assert initial_iteration is not None
    assert initial_iteration.workflow_id == delegated_request.id
    assert initial_graph is not None
    assert initial_graph.iteration_id == initial_iteration.id
    assert parent_task is not None
    assert parent_task["status"] == TaskCenterTaskStatus.WAITING_WORKFLOW.value
    # Delegated orchestrator was started.
    assert runtime.orchestrator_registry.get(initial_graph.id) is not None


def test_workflow_start_startup_failure_leaves_parent_running(
    workflow_store, iteration_store, attempt_store, task_store, task_center_run_id, composer
) -> None:
    runtime = _build_runtime(
        workflow_store, iteration_store, attempt_store, task_store, composer=composer
    )
    parent_task_id, parent_attempt_id = _seed_outer_generator_task(
        task_store=task_store,
        workflow_store=workflow_store,
        iteration_store=iteration_store,
        attempt_store=attempt_store,
        task_center_run_id=task_center_run_id,
    )

    def _failing_factory(attempt, on_attempt_closed):
        del attempt, on_attempt_closed
        raise RuntimeError("delegated startup boom")

    starter = WorkflowStarter(runtime=runtime, orchestrator_factory=_failing_factory)
    with pytest.raises(RuntimeError):
        starter.start(
            prompt="delegated",
            origin=WorkflowOrigin.task(task_id=parent_task_id),
        )

    parent_task = task_store.get_task(parent_task_id)
    assert parent_task is not None
    assert parent_task["status"] == TaskCenterTaskStatus.RUNNING.value
    # The compensation path must mark the request and iteration cancelled.
    open_requests = [
        r
        for r in workflow_store.list_for_parent_task(parent_task_id)
        if r.is_open
    ]
    assert open_requests == []
    cancelled = [
        r
        for r in workflow_store.list_for_parent_task(parent_task_id)
        if r.status == WorkflowStatus.CANCELLED
    ]
    assert len(cancelled) == 1
    assert cancelled[0].requested_by_task_id == parent_task_id
    cancelled_segment = iteration_store.list_for_workflow(cancelled[0].id)
    assert len(cancelled_segment) == 1
    assert cancelled_segment[0].status == IterationStatus.CANCELLED
    assert runtime.iteration_coordinators is not None
    assert runtime.iteration_coordinators.get(cancelled_segment[0].id) is None


def test_workflow_start_startup_failure_closes_started_graph_and_deregisters_orchestrator(
    workflow_store, iteration_store, attempt_store, task_store, task_center_run_id, composer
) -> None:
    runtime = _build_runtime(
        workflow_store,
        iteration_store,
        attempt_store,
        task_store,
        launcher=_FailingLauncher(),
        composer=composer,
    )
    parent_task_id, parent_attempt_id = _seed_outer_generator_task(
        task_store=task_store,
        workflow_store=workflow_store,
        iteration_store=iteration_store,
        attempt_store=attempt_store,
        task_center_run_id=task_center_run_id,
    )
    coordinator = WorkflowStarter(runtime=runtime)

    with pytest.raises(RuntimeError):
        coordinator.start(
            prompt="delegated",
            origin=WorkflowOrigin.task(task_id=parent_task_id),
        )

    [cancelled_request] = [
        r
        for r in workflow_store.list_for_parent_task(parent_task_id)
        if r.status == WorkflowStatus.CANCELLED
    ]
    [cancelled_segment] = iteration_store.list_for_workflow(cancelled_request.id)
    [failed_attempt] = attempt_store.list_for_iteration(cancelled_segment.id)
    assert failed_attempt.status == AttemptStatus.FAILED
    assert failed_attempt.fail_reason == AttemptFailReason.STARTUP_FAILED
    assert runtime.orchestrator_registry.get(failed_attempt.id) is None
    assert runtime.iteration_coordinators is not None
    assert runtime.iteration_coordinators.get(cancelled_segment.id) is None
    planner_task = task_store.get_task(planner_task_id(failed_attempt.id))
    assert planner_task is not None
    assert planner_task["status"] == TaskCenterTaskStatus.FAILED.value


def test_workflow_start_rejects_second_open_child_request_for_same_executor(
    workflow_store, iteration_store, attempt_store, task_store, task_center_run_id, composer
) -> None:
    runtime = _build_runtime(
        workflow_store, iteration_store, attempt_store, task_store, composer=composer
    )
    parent_task_id, parent_attempt_id = _seed_outer_generator_task(
        task_store=task_store,
        workflow_store=workflow_store,
        iteration_store=iteration_store,
        attempt_store=attempt_store,
        task_center_run_id=task_center_run_id,
    )
    coordinator = WorkflowStarter(runtime=runtime)
    coordinator.start(
        prompt="first delegation",
        origin=WorkflowOrigin.task(task_id=parent_task_id),
    )

    # Restore the parent to running so the second call passes the running gate
    # but is rejected by the duplicate-open-request check.
    task_store.set_task_status(
        parent_task_id,
        status=TaskCenterTaskStatus.RUNNING.value,
    )

    with pytest.raises(TaskCenterInvariantViolation) as exc:
        coordinator.start(
            prompt="second delegation",
            origin=WorkflowOrigin.task(task_id=parent_task_id),
        )
    assert "open delegated workflow" in str(exc.value)


def test_workflow_start_rejects_non_running_parent(
    workflow_store, iteration_store, attempt_store, task_store, task_center_run_id, composer
) -> None:
    runtime = _build_runtime(
        workflow_store, iteration_store, attempt_store, task_store, composer=composer
    )
    parent_task_id, parent_attempt_id = _seed_outer_generator_task(
        task_store=task_store,
        workflow_store=workflow_store,
        iteration_store=iteration_store,
        attempt_store=attempt_store,
        task_center_run_id=task_center_run_id,
    )
    task_store.set_task_status(
        parent_task_id, status=TaskCenterTaskStatus.DONE.value
    )

    coordinator = WorkflowStarter(runtime=runtime)
    with pytest.raises(TaskCenterInvariantViolation) as exc:
        coordinator.start(
            prompt="delegated",
            origin=WorkflowOrigin.task(task_id=parent_task_id),
        )
    assert "not running" in str(exc.value)


def test_workflow_start_accepts_entry_origin_without_parent_task(
    workflow_store, iteration_store, attempt_store, task_store, task_center_run_id, composer
) -> None:
    iteration_coordinators = OpenIterationCoordinatorRegistry()
    runtime = AttemptDeps(
        workflow_store=workflow_store,
        iteration_store=iteration_store,
        attempt_store=attempt_store,
        task_store=task_store,
        agent_launcher=_FakeLauncher(),
        orchestrator_registry=AttemptOrchestratorRegistry(),
        iteration_coordinators=iteration_coordinators,
        composer=composer,
    )

    coordinator = WorkflowStarter(runtime=runtime)
    result: StartedWorkflow = coordinator.start(
        prompt="solve entry prompt",
        origin=WorkflowOrigin.entry(task_center_run_id=task_center_run_id),
    )

    assert result.origin.kind == WorkflowOriginKind.ENTRY
    assert result.parent_task_id is None
    assert result.parent_attempt_id is None
    delegated_request = workflow_store.get(result.workflow_id)
    delegated_segment = iteration_store.get(result.initial_iteration_id)
    delegated_attempt = attempt_store.get(result.initial_attempt_id)
    assert delegated_request is not None
    assert delegated_request.origin_kind == WorkflowOriginKind.ENTRY
    assert delegated_request.requested_by_task_id is None
    assert delegated_request.goal == "solve entry prompt"
    assert delegated_request.status == WorkflowStatus.OPEN
    assert delegated_segment is not None
    assert delegated_attempt is not None
    assert runtime.orchestrator_registry.get(delegated_attempt.id) is not None
