"""WorkflowLifecycle lifecycle tests covering Phase 01 exit criteria."""

from __future__ import annotations

import pytest

from task_center._core.primitives import TaskCenterLifecycleConfig
from task_center.workflow.lifecycle import WorkflowLifecycle
from task_center.iteration import OpenIterationCoordinatorRegistry
from task_center.workflow.state import WorkflowOrigin, WorkflowStatus
from task_center.iteration.state import (
    AttemptPlanFailed,
    SuccessDeferred,
    IterationClosureReport,
    TerminalSuccess,
)
from task_center.iteration.state import (
    IterationCreationReason,
    IterationStatus,
)
from task_center._core.primitives import TaskCenterInvariantViolation


@pytest.fixture
def iteration_coordinators():
    return OpenIterationCoordinatorRegistry()


@pytest.fixture
def workflow_lifecycle(workflow_store, iteration_store, attempt_store, iteration_coordinators):
    return WorkflowLifecycle(
        workflow_store=workflow_store,
        iteration_store=iteration_store,
        attempt_store=attempt_store,
        iteration_coordinators=iteration_coordinators,
        config=TaskCenterLifecycleConfig(default_attempt_budget=2),
    )


def test_create_goal_links_executor(
    workflow_lifecycle, workflow_store, task_center_run_id
):
    """Phase 01 exit: delegated workflow links to requested_by_task_id."""
    goal = workflow_lifecycle.create_workflow(
        task_center_run_id=task_center_run_id,
        origin=WorkflowOrigin.task(task_id="executor-1"),
        goal="solve X",
    )
    assert goal.requested_by_task_id == "executor-1"
    assert goal.task_center_run_id == task_center_run_id
    assert goal.is_open
    assert goal.iteration_ids == ()
    persisted = workflow_store.get(goal.id)
    assert persisted is not None
    assert persisted.requested_by_task_id == "executor-1"


def test_goal_records_iterations_in_iteration_ids(
    workflow_lifecycle, workflow_store, task_center_run_id
):
    """Phase 01 exit: each goal records created iterations in iteration_ids."""
    goal = workflow_lifecycle.create_workflow(
        task_center_run_id=task_center_run_id,
        origin=WorkflowOrigin.task(task_id="t1"),
        goal="g",
    )
    iteration, _ = workflow_lifecycle.create_initial_iteration_with_coordinator(workflow_id=goal.id)
    refreshed = workflow_store.get(goal.id)
    assert refreshed is not None
    assert refreshed.iteration_ids == (iteration.id,)


def test_initial_iteration_has_sequence_one_and_initial_reason(workflow_lifecycle, task_center_run_id):
    goal = workflow_lifecycle.create_workflow(
        task_center_run_id=task_center_run_id,
        origin=WorkflowOrigin.task(task_id="t1"),
        goal="g",
    )
    iteration, _ = workflow_lifecycle.create_initial_iteration_with_coordinator(workflow_id=goal.id)
    assert iteration.sequence_no == 1
    assert iteration.creation_reason == IterationCreationReason.INITIAL
    assert iteration.goal == "g"
    assert iteration.is_open
    assert iteration.attempt_budget == 2


def test_continuation_iteration_inherits_deferred_goal(
    workflow_lifecycle, iteration_store, task_center_run_id
):
    """Phase 01 exit: continuation inherits the predecessor's deferred goal."""
    goal = workflow_lifecycle.create_workflow(
        task_center_run_id=task_center_run_id,
        origin=WorkflowOrigin.task(task_id="t1"),
        goal="initial-goal",
    )
    iteration1, _ = workflow_lifecycle.create_initial_iteration_with_coordinator(workflow_id=goal.id)
    # Mark predecessor SUCCEEDED with a deferred_goal_for_next_iteration so the invariant passes.
    iteration_store.set_deferred_goal_for_next_iteration(iteration1.id, "next-goal")
    iteration_store.set_status(iteration1.id, status=IterationStatus.SUCCEEDED)
    iteration1_succeeded = iteration_store.get(iteration1.id)
    assert iteration1_succeeded is not None

    iteration2, _ = workflow_lifecycle.create_deferred_iteration_with_coordinator(
        previous_iteration=iteration1_succeeded
    )
    assert iteration2.sequence_no == 2
    assert iteration2.creation_reason == IterationCreationReason.DEFERRED_GOAL_CONTINUATION
    assert iteration2.goal == "next-goal"


def test_iteration_ids_holds_multiple_iterations(
    workflow_lifecycle, workflow_store, iteration_store, task_center_run_id
):
    """Phase 01 exit: iteration_ids can hold multiple Iteration ids for one goal."""
    goal = workflow_lifecycle.create_workflow(
        task_center_run_id=task_center_run_id,
        origin=WorkflowOrigin.task(task_id="t1"),
        goal="g1",
    )
    iteration1, _ = workflow_lifecycle.create_initial_iteration_with_coordinator(workflow_id=goal.id)
    iteration_store.set_deferred_goal_for_next_iteration(iteration1.id, "g2")
    iteration_store.set_status(iteration1.id, status=IterationStatus.SUCCEEDED)
    iteration1_succeeded = iteration_store.get(iteration1.id)
    assert iteration1_succeeded is not None
    iteration2, _ = workflow_lifecycle.create_deferred_iteration_with_coordinator(
        previous_iteration=iteration1_succeeded
    )
    refreshed = workflow_store.get(goal.id)
    assert refreshed is not None
    assert refreshed.iteration_ids == (iteration1.id, iteration2.id)


def test_handle_iteration_closed_terminal_success_closes_goal_succeeded(
    workflow_lifecycle, workflow_store, iteration_store, task_center_run_id
):
    goal = workflow_lifecycle.create_workflow(
        task_center_run_id=task_center_run_id,
        origin=WorkflowOrigin.task(task_id="t1"),
        goal="g",
    )
    iteration, _ = workflow_lifecycle.create_initial_iteration_with_coordinator(workflow_id=goal.id)
    workflow_lifecycle.handle_iteration_closed(
        IterationClosureReport(
            iteration_id=iteration.id,
            final_attempt_id="g1",
            outcome=TerminalSuccess(),
        )
    )
    final = workflow_store.get(goal.id)
    assert final is not None
    assert final.status == WorkflowStatus.SUCCEEDED
    assert final.final_outcome == {
        "outcome": "success",
        "final_iteration_id": iteration.id,
        "final_attempt_id": "g1",
    }


def test_handle_iteration_closed_attempt_plan_failed_closes_goal_failed(
    workflow_lifecycle, workflow_store, task_center_run_id
):
    goal = workflow_lifecycle.create_workflow(
        task_center_run_id=task_center_run_id,
        origin=WorkflowOrigin.task(task_id="t1"),
        goal="g",
    )
    iteration, _ = workflow_lifecycle.create_initial_iteration_with_coordinator(workflow_id=goal.id)
    workflow_lifecycle.handle_iteration_closed(
        IterationClosureReport(
            iteration_id=iteration.id,
            final_attempt_id="g1",
            outcome=AttemptPlanFailed(
                failure_summary="boom", prior_attempt_history=()
            ),
        )
    )
    final = workflow_store.get(goal.id)
    assert final is not None
    assert final.status == WorkflowStatus.FAILED


def test_handle_iteration_closed_success_continue_creates_continuation(
    workflow_lifecycle, workflow_store, iteration_store, task_center_run_id
):
    goal = workflow_lifecycle.create_workflow(
        task_center_run_id=task_center_run_id,
        origin=WorkflowOrigin.task(task_id="t1"),
        goal="g",
    )
    iteration1, _ = workflow_lifecycle.create_initial_iteration_with_coordinator(workflow_id=goal.id)
    iteration_store.set_deferred_goal_for_next_iteration(iteration1.id, "next-goal")
    iteration_store.set_status(iteration1.id, status=IterationStatus.SUCCEEDED)
    workflow_lifecycle.handle_iteration_closed(
        IterationClosureReport(
            iteration_id=iteration1.id,
            final_attempt_id="g1",
            outcome=SuccessDeferred(deferred_goal_for_next_iteration="next-goal"),
        )
    )
    refreshed = workflow_store.get(goal.id)
    assert refreshed is not None
    assert len(refreshed.iteration_ids) == 2
    iteration2_id = refreshed.iteration_ids[1]
    iteration2 = iteration_store.get(iteration2_id)
    assert iteration2 is not None
    assert iteration2.sequence_no == 2
    assert iteration2.goal == "next-goal"


def test_handle_iteration_closed_deregisters_coordinator(
    workflow_lifecycle, iteration_coordinators, task_center_run_id
):
    goal = workflow_lifecycle.create_workflow(
        task_center_run_id=task_center_run_id,
        origin=WorkflowOrigin.task(task_id="t1"),
        goal="g",
    )
    iteration, _ = workflow_lifecycle.create_initial_iteration_with_coordinator(workflow_id=goal.id)
    assert iteration_coordinators.get(iteration.id) is not None
    workflow_lifecycle.handle_iteration_closed(
        IterationClosureReport(
            iteration_id=iteration.id,
            final_attempt_id="g1",
            outcome=TerminalSuccess(),
        )
    )
    assert iteration_coordinators.get(iteration.id) is None


def test_continuation_iteration_only_from_succeeded_predecessor_with_goal(
    workflow_lifecycle, iteration_store, task_center_run_id
):
    goal = workflow_lifecycle.create_workflow(
        task_center_run_id=task_center_run_id,
        origin=WorkflowOrigin.task(task_id="t1"),
        goal="g",
    )
    iteration1, _ = workflow_lifecycle.create_initial_iteration_with_coordinator(workflow_id=goal.id)

    # Predecessor still OPEN -> invariant violation.
    with pytest.raises(TaskCenterInvariantViolation):
        workflow_lifecycle.create_deferred_iteration_with_coordinator(previous_iteration=iteration1)

    # Predecessor SUCCEEDED but no deferred_goal_for_next_iteration -> invariant violation.
    iteration_store.set_status(iteration1.id, status=IterationStatus.SUCCEEDED)
    iteration1_no_goal = iteration_store.get(iteration1.id)
    assert iteration1_no_goal is not None
    with pytest.raises(TaskCenterInvariantViolation):
        workflow_lifecycle.create_deferred_iteration_with_coordinator(previous_iteration=iteration1_no_goal)


def test_open_iteration_coordinators_enforces_unique_per_iteration(
    workflow_lifecycle, task_center_run_id
):
    """Phase 01 spec: exactly one IterationAttemptCoordinator active per open iteration."""
    goal = workflow_lifecycle.create_workflow(
        task_center_run_id=task_center_run_id,
        origin=WorkflowOrigin.task(task_id="t1"),
        goal="g",
    )
    workflow_lifecycle.create_initial_iteration_with_coordinator(workflow_id=goal.id)
    # Calling create_initial_iteration again should fail because the goal now
    # has iteration 1 — sequence_no 1 is no longer the contiguous next.
    with pytest.raises(TaskCenterInvariantViolation):
        workflow_lifecycle.create_initial_iteration_with_coordinator(workflow_id=goal.id)


def test_close_goal_delivers_closure_report_when_callback_set(
    workflow_store, iteration_store, attempt_store, task_center_run_id
):
    delivered: list = []

    def sink(report) -> None:
        delivered.append(report)

    workflow_lifecycle = WorkflowLifecycle(
        workflow_store=workflow_store,
        iteration_store=iteration_store,
        attempt_store=attempt_store,
        iteration_coordinators=OpenIterationCoordinatorRegistry(),
        config=TaskCenterLifecycleConfig(default_attempt_budget=2),
        deliver_closure_report=sink,
    )
    goal = workflow_lifecycle.create_workflow(
        task_center_run_id=task_center_run_id,
        origin=WorkflowOrigin.task(task_id="executor-1"),
        goal="g",
    )
    workflow_lifecycle.create_initial_iteration_with_coordinator(workflow_id=goal.id)
    workflow_lifecycle.close_workflow(
        workflow_id=goal.id,
        succeeded=True,
        final_iteration_id="iteration",
        final_attempt_id="g1",
    )
    assert len(delivered) == 1
    assert delivered[0].outcome == "success"
    assert delivered[0].requested_by_task_id == "executor-1"


def test_goal_lifecycle_passes_orchestrator_factory_to_spawned_coordinator(
    workflow_store, iteration_store, attempt_store, task_center_run_id
):
    started: list[str] = []

    class _StartedOrchestrator:
        def __init__(self, attempt_id: str) -> None:
            self.attempt_id = attempt_id

        def start(self) -> None:
            started.append(self.attempt_id)

    def factory(attempt, on_attempt_closed):
        del on_attempt_closed
        return _StartedOrchestrator(attempt.id)

    registry = OpenIterationCoordinatorRegistry()
    workflow_lifecycle = WorkflowLifecycle(
        workflow_store=workflow_store,
        iteration_store=iteration_store,
        attempt_store=attempt_store,
        iteration_coordinators=registry,
        config=TaskCenterLifecycleConfig(default_attempt_budget=2),
        orchestrator_factory=factory,
    )
    goal = workflow_lifecycle.create_workflow(
        task_center_run_id=task_center_run_id,
        origin=WorkflowOrigin.task(task_id="executor-1"),
        goal="g",
    )
    iteration, _ = workflow_lifecycle.create_initial_iteration_with_coordinator(workflow_id=goal.id)
    coordinator = registry.get(iteration.id)
    assert coordinator is not None

    attempt = coordinator.create_initial_attempt()

    assert started == [attempt.id]


def test_no_legacy_entry_creation_reason_in_lifecycle(workflow_lifecycle, task_center_run_id):
    """Phase 01 spec: no special entry creation reason is allowed."""
    # Indirect: goal-lifecycle driven iteration creation only ever uses INITIAL or
    # DEFERRED_GOAL_CONTINUATION. There is no public path that produces a
    # special entry-only iteration reason.
    goal = workflow_lifecycle.create_workflow(
        task_center_run_id=task_center_run_id,
        origin=WorkflowOrigin.task(task_id="t1"),
        goal="g",
    )
    iteration, _ = workflow_lifecycle.create_initial_iteration_with_coordinator(workflow_id=goal.id)
    assert iteration.creation_reason in (
        IterationCreationReason.INITIAL,
        IterationCreationReason.DEFERRED_GOAL_CONTINUATION,
    )
