"""Workflow lifecycle closure smoke with a synchronous attempt closer."""

from __future__ import annotations

from collections.abc import Callable

from db.stores.attempt_store import AttemptStore
from task_center._core.primitives import TaskCenterLifecycleConfig
from task_center.workflow.lifecycle import WorkflowLifecycle
from task_center.iteration import (
    IterationAttemptCoordinator,
    OpenIterationCoordinatorRegistry,
)
from task_center.workflow.state import WorkflowOrigin, WorkflowStatus
from task_center.attempt import (
    Attempt,
    AttemptFailReason,
    AttemptStatus,
)
from task_center.iteration.state import IterationStatus


class _StubOrchestrator:
    """Synchronous stand-in for AttemptOrchestrator.

    Closes the attempt immediately on ``start`` with a caller-supplied verdict.
    """

    def __init__(
        self,
        *,
        attempt: Attempt,
        attempt_store: AttemptStore,
        on_attempt_closed: Callable[[str], None],
        verdict: tuple[
            AttemptStatus, AttemptFailReason | None, str | None
        ],
    ) -> None:
        self._g = attempt
        self._gs = attempt_store
        self._cb = on_attempt_closed
        self._verdict = verdict

    def start(self) -> None:
        status, fail_reason, deferred_goal_for_next_iteration = self._verdict
        if deferred_goal_for_next_iteration is not None:
            self._gs.set_plan_contract(
                self._g.id,
                plan_spec="stub-spec",
                evaluation_criteria=["stub-criterion"],
                deferred_goal_for_next_iteration=deferred_goal_for_next_iteration,
            )
        self._gs.close(self._g.id, status=status, fail_reason=fail_reason)
        self._cb(self._g.id)


def _build_workflow_lifecycle(workflow_store, iteration_store, attempt_store):
    iteration_coordinators = OpenIterationCoordinatorRegistry()
    workflow_lifecycle = WorkflowLifecycle(
        workflow_store=workflow_store,
        iteration_store=iteration_store,
        attempt_store=attempt_store,
        iteration_coordinators=iteration_coordinators,
        config=TaskCenterLifecycleConfig(default_attempt_budget=2),
    )
    return workflow_lifecycle, iteration_coordinators


def _drive_segment(
    *,
    iteration_coordinators: OpenIterationCoordinatorRegistry,
    iteration_id: str,
    attempt_store: AttemptStore,
    verdict: tuple[
        AttemptStatus, AttemptFailReason | None, str | None
    ],
) -> None:
    """Run a stub orchestrator against the coordinator-owned iteration."""
    coordinator: IterationAttemptCoordinator | None = iteration_coordinators.get(
        iteration_id
    )
    assert coordinator is not None
    g = coordinator.create_initial_attempt()
    stub = _StubOrchestrator(
        attempt=g,
        attempt_store=attempt_store,
        on_attempt_closed=coordinator.handle_attempt_closed,
        verdict=verdict,
    )
    stub.start()


def test_smoke_terminal_success(
    workflow_store, iteration_store, attempt_store, task_center_run_id
):
    workflow_lifecycle, iteration_coordinators = _build_workflow_lifecycle(
        workflow_store, iteration_store, attempt_store
    )
    req = workflow_lifecycle.create_workflow(
        task_center_run_id=task_center_run_id,
        origin=WorkflowOrigin.task(task_id="exec-1"),
        goal="solve X",
    )
    seg, _ = workflow_lifecycle.create_initial_iteration_with_coordinator(workflow_id=req.id)
    _drive_segment(
        iteration_coordinators=iteration_coordinators,
        iteration_id=seg.id,
        attempt_store=attempt_store,
        verdict=(AttemptStatus.PASSED, None, None),
    )
    final_request = workflow_store.get(req.id)
    final_segment = iteration_store.get(seg.id)
    assert final_request is not None and final_segment is not None
    assert final_request.status == WorkflowStatus.SUCCEEDED
    assert final_segment.status == IterationStatus.SUCCEEDED


def test_smoke_attempt_plan_failed(
    workflow_store, iteration_store, attempt_store, task_center_run_id
):
    workflow_lifecycle, iteration_coordinators = _build_workflow_lifecycle(
        workflow_store, iteration_store, attempt_store
    )
    req = workflow_lifecycle.create_workflow(
        task_center_run_id=task_center_run_id,
        origin=WorkflowOrigin.task(task_id="exec-1"),
        goal="solve X",
    )
    seg, _ = workflow_lifecycle.create_initial_iteration_with_coordinator(workflow_id=req.id)
    # First attempt: fail with a generator error.
    coordinator = iteration_coordinators.get(seg.id)
    assert coordinator is not None
    g1 = coordinator.create_initial_attempt()
    attempt_store.set_plan_contract(
        g1.id, plan_spec="spec1", evaluation_criteria=["a"], deferred_goal_for_next_iteration=None
    )
    attempt_store.close(
        g1.id, status=AttemptStatus.FAILED,
        fail_reason=AttemptFailReason.GENERATOR_FAILED,
    )
    coordinator.handle_attempt_closed(g1.id)
    # Second (and budget-final) attempt: also fail.
    seg_after = iteration_store.get(seg.id)
    assert seg_after is not None
    g2_id = seg_after.attempt_ids[-1]
    attempt_store.set_plan_contract(
        g2_id, plan_spec="spec2", evaluation_criteria=["b"], deferred_goal_for_next_iteration=None
    )
    attempt_store.close(
        g2_id, status=AttemptStatus.FAILED,
        fail_reason=AttemptFailReason.EVALUATOR_FAILED,
    )
    coordinator.handle_attempt_closed(g2_id)
    final_request = workflow_store.get(req.id)
    final_segment = iteration_store.get(seg.id)
    assert final_request is not None and final_segment is not None
    assert final_request.status == WorkflowStatus.FAILED
    assert final_segment.status == IterationStatus.FAILED
    assert final_request.final_outcome is not None
    assert final_request.final_outcome["outcome"] == "failed"


def test_smoke_success_continue_then_terminal(
    workflow_store, iteration_store, attempt_store, task_center_run_id
):
    workflow_lifecycle, iteration_coordinators = _build_workflow_lifecycle(
        workflow_store, iteration_store, attempt_store
    )
    req = workflow_lifecycle.create_workflow(
        task_center_run_id=task_center_run_id,
        origin=WorkflowOrigin.task(task_id="exec-1"),
        goal="initial-goal",
    )
    seg1, _ = workflow_lifecycle.create_initial_iteration_with_coordinator(workflow_id=req.id)
    _drive_segment(
        iteration_coordinators=iteration_coordinators,
        iteration_id=seg1.id,
        attempt_store=attempt_store,
        verdict=(AttemptStatus.PASSED, None, "next-goal"),
    )
    refreshed = workflow_store.get(req.id)
    assert refreshed is not None
    assert len(refreshed.iteration_ids) == 2
    assert refreshed.is_open
    seg2_id = refreshed.iteration_ids[1]
    seg2 = iteration_store.get(seg2_id)
    assert seg2 is not None
    assert seg2.goal == "next-goal"
    # Drive iteration 2 to terminal success.
    _drive_segment(
        iteration_coordinators=iteration_coordinators,
        iteration_id=seg2_id,
        attempt_store=attempt_store,
        verdict=(AttemptStatus.PASSED, None, None),
    )
    final_request = workflow_store.get(req.id)
    assert final_request is not None
    assert final_request.status == WorkflowStatus.SUCCEEDED
