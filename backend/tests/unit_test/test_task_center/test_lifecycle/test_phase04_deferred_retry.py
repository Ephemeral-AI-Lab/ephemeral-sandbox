"""Phase 04 end-to-end continuation and retry tests.

Drives the full starter → goal lifecycle → iteration coordinator → orchestrator pipeline so
that retry, continuation, and final close-report routing are exercised
together. The parent task must remain in ``waiting_workflow`` until the
delegated workflow closes terminally.
"""

from __future__ import annotations

from task_center.workflow.starter import WorkflowStarter
from task_center.workflow.state import WorkflowOrigin, WorkflowStatus
from task_center.attempt.orchestrator import AttemptOrchestrator
from task_center.attempt.orchestrator_registry import (
    AttemptOrchestratorRegistry,
)
from task_center.attempt import (
    AttemptFailReason,
    AttemptStatus,
)
from task_center.attempt.deps import AgentLaunch, AttemptDeps
from task_center.iteration import OpenIterationCoordinatorRegistry
from task_center.iteration.state import (
    IterationCreationReason,
    IterationStatus,
)
from task_center._core.task_state import TaskCenterTaskStatus
from task_center.submissions import EvaluatorSubmission, GeneratorSubmission, PlannedGeneratorTask, PlannerSubmission
from task_center._core.primitives import evaluator_task_id, generator_task_id, planner_task_id


class _FakeLauncher:
    def __init__(self) -> None:
        self.launches: list[AgentLaunch] = []

    def launch(self, launch: AgentLaunch) -> None:
        self.launches.append(launch)


class _FailOnLaunchNumber(_FakeLauncher):
    def __init__(self, fail_on: int) -> None:
        super().__init__()
        self._fail_on = fail_on

    def launch(self, launch: AgentLaunch) -> None:
        super().launch(launch)
        if len(self.launches) == self._fail_on:
            raise RuntimeError("planned launch failure")


def _build_runtime(
    workflow_store, iteration_store, attempt_store, task_store, *, composer, launcher=None
) -> AttemptDeps:
    return AttemptDeps(
        workflow_store=workflow_store,
        iteration_store=iteration_store,
        attempt_store=attempt_store,
        task_store=task_store,
        agent_launcher=launcher or _FakeLauncher(),
        orchestrator_registry=AttemptOrchestratorRegistry(),
        iteration_coordinators=OpenIterationCoordinatorRegistry(),
        composer=composer,
    )


def _seed_outer_running_generator(
    *,
    runtime: AttemptDeps,
    workflow_store,
    iteration_store,
    attempt_store,
    task_store,
    task_center_run_id: str,
) -> tuple[str, str]:
    """Seed an outer parent attempt + a single running generator task on it."""
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
    outer_orchestrator = AttemptOrchestrator(
        attempt=outer_attempt,
        on_attempt_closed=lambda attempt_id: None,
        runtime=runtime,
    )
    runtime.orchestrator_registry.register(outer_orchestrator)
    outer_orchestrator.start()
    outer_orchestrator.apply_plan_submission(
        PlannerSubmission(
            attempt_id=outer_attempt.id,
            planner_task_id=planner_task_id(outer_attempt.id),
            kind="completes",
            plan_spec="outer spec",
            evaluation_criteria=("outer ok",),
            tasks=(
                PlannedGeneratorTask(
                    local_id="outer",
                    agent_name="executor",
                    deps=(),
                    task_spec="execute outer",
                ),
            ),
            deferred_goal_for_next_iteration=None,
            summary="outer plan",
        )
    )
    parent_task_id = generator_task_id(outer_attempt.id, "outer")
    return parent_task_id, outer_attempt.id


def _drive_delegated_attempt_to_pass(
    *,
    runtime: AttemptDeps,
    delegated_attempt_id: str,
    deferred_goal_for_next_iteration: str | None,
) -> None:
    """Plan, execute, and pass the delegated attempt."""
    delegated = runtime.orchestrator_registry.get_or_raise(delegated_attempt_id)
    if deferred_goal_for_next_iteration is None:
        delegated.apply_plan_submission(
            PlannerSubmission(
                attempt_id=delegated_attempt_id,
                planner_task_id=planner_task_id(delegated_attempt_id),
                kind="completes",
                plan_spec="delegated spec",
                evaluation_criteria=("delegated ok",),
                tasks=(
                    PlannedGeneratorTask(
                        local_id="d",
                        agent_name="executor",
                        deps=(),
                        task_spec="do delegated",
                    ),
                ),
                deferred_goal_for_next_iteration=None,
                summary="delegated plan",
            )
        )
    else:
        delegated.apply_plan_submission(
            PlannerSubmission(
                attempt_id=delegated_attempt_id,
                planner_task_id=planner_task_id(delegated_attempt_id),
                kind="defers",
                plan_spec="delegated spec",
                evaluation_criteria=("delegated ok",),
                tasks=(
                    PlannedGeneratorTask(
                        local_id="d",
                        agent_name="executor",
                        deps=(),
                        task_spec="do delegated",
                    ),
                ),
                deferred_goal_for_next_iteration=deferred_goal_for_next_iteration,
                summary="delegated plan",
            )
        )
    delegated.apply_generator_submission(
        GeneratorSubmission(
            attempt_id=delegated_attempt_id,
            task_id=generator_task_id(delegated_attempt_id, "d"),
            outcome="success",
            summary="generator ok",
            payload={},
        )
    )
    delegated.apply_evaluator_submission(
        EvaluatorSubmission(
            attempt_id=delegated_attempt_id,
            task_id=evaluator_task_id(delegated_attempt_id),
            outcome="success",
            summary="evaluator ok",
            payload={},
        )
    )


def _drive_delegated_attempt_to_fail(
    *,
    runtime: AttemptDeps,
    delegated_attempt_id: str,
) -> None:
    delegated = runtime.orchestrator_registry.get_or_raise(delegated_attempt_id)
    delegated.apply_plan_submission(
        PlannerSubmission(
            attempt_id=delegated_attempt_id,
            planner_task_id=planner_task_id(delegated_attempt_id),
            kind="completes",
            plan_spec="delegated spec",
            evaluation_criteria=("delegated ok",),
            tasks=(
                PlannedGeneratorTask(
                    local_id="d",
                    agent_name="executor",
                    deps=(),
                    task_spec="do delegated",
                ),
            ),
            deferred_goal_for_next_iteration=None,
            summary="delegated plan",
        )
    )
    delegated.apply_generator_submission(
        GeneratorSubmission(
            attempt_id=delegated_attempt_id,
            task_id=generator_task_id(delegated_attempt_id, "d"),
            outcome="failure",
            summary="generator failed",
            payload={},
        )
    )


def test_delegated_continuation_waits_until_final_segment(
    workflow_store, iteration_store, attempt_store, task_store, task_center_run_id, composer
) -> None:
    runtime = _build_runtime(
        workflow_store, iteration_store, attempt_store, task_store, composer=composer
    )
    parent_task_id, parent_attempt_id = _seed_outer_running_generator(
        runtime=runtime,
        workflow_store=workflow_store,
        iteration_store=iteration_store,
        attempt_store=attempt_store,
        task_store=task_store,
        task_center_run_id=task_center_run_id,
    )
    coordinator = WorkflowStarter(runtime=runtime)
    workflow_start = coordinator.start(
        prompt="delegated continuation",
        origin=WorkflowOrigin.task(task_id=parent_task_id),
    )

    segment1_initial_attempt_id = workflow_start.initial_attempt_id

    # Segment 1 passes with continuation goal — parent must remain WAITING.
    _drive_delegated_attempt_to_pass(
        runtime=runtime,
        delegated_attempt_id=segment1_initial_attempt_id,
        deferred_goal_for_next_iteration="continue work",
    )
    parent_after_segment1 = task_store.get_task(parent_task_id)
    assert parent_after_segment1 is not None
    assert (
        parent_after_segment1["status"]
        == TaskCenterTaskStatus.WAITING_WORKFLOW.value
    )
    delegated_request_after_segment1 = workflow_store.get(
        workflow_start.workflow_id
    )
    assert delegated_request_after_segment1 is not None
    assert delegated_request_after_segment1.status == WorkflowStatus.OPEN
    assert len(delegated_request_after_segment1.iteration_ids) == 2

    # Segment 2 starts from the new continuation attempt the goal lifecycle created.
    segment2_id = delegated_request_after_segment1.iteration_ids[1]
    segment2 = iteration_store.get(segment2_id)
    assert segment2 is not None
    assert segment2.goal == "continue work"
    segment2_initial_attempt_id = segment2.attempt_ids[0]
    # Drive iteration 2 to terminal success.
    _drive_delegated_attempt_to_pass(
        runtime=runtime,
        delegated_attempt_id=segment2_initial_attempt_id,
        deferred_goal_for_next_iteration=None,
    )

    parent_final = task_store.get_task(parent_task_id)
    delegated_final = workflow_store.get(workflow_start.workflow_id)
    segment2_final = iteration_store.get(segment2_id)
    assert parent_final is not None
    assert parent_final["status"] == TaskCenterTaskStatus.DONE.value
    assert delegated_final is not None
    assert delegated_final.status == WorkflowStatus.SUCCEEDED
    assert segment2_final is not None
    assert segment2_final.status == IterationStatus.SUCCEEDED


def test_continuation_startup_failure_reports_continuation_graph(
    workflow_store, iteration_store, attempt_store, task_store, task_center_run_id, composer
) -> None:
    launcher = _FailOnLaunchNumber(fail_on=6)
    runtime = _build_runtime(
        workflow_store,
        iteration_store,
        attempt_store,
        task_store,
        composer=composer,
        launcher=launcher,
    )
    parent_task_id, parent_attempt_id = _seed_outer_running_generator(
        runtime=runtime,
        workflow_store=workflow_store,
        iteration_store=iteration_store,
        attempt_store=attempt_store,
        task_store=task_store,
        task_center_run_id=task_center_run_id,
    )
    coordinator = WorkflowStarter(runtime=runtime)
    workflow_start = coordinator.start(
        prompt="delegated continuation",
        origin=WorkflowOrigin.task(task_id=parent_task_id),
    )

    _drive_delegated_attempt_to_pass(
        runtime=runtime,
        delegated_attempt_id=workflow_start.initial_attempt_id,
        deferred_goal_for_next_iteration="continue work",
    )

    request = workflow_store.get(workflow_start.workflow_id)
    assert request is not None
    assert request.status == WorkflowStatus.FAILED
    assert request.final_outcome is not None
    segment2_id = request.iteration_ids[1]
    segment2 = iteration_store.get(segment2_id)
    assert segment2 is not None
    failed_attempt_id = segment2.attempt_ids[0]
    failed_attempt = attempt_store.get(failed_attempt_id)
    assert failed_attempt is not None
    assert failed_attempt.status == AttemptStatus.FAILED
    assert failed_attempt.fail_reason == AttemptFailReason.STARTUP_FAILED
    assert request.final_outcome["final_iteration_id"] == segment2_id
    assert request.final_outcome["final_attempt_id"] == failed_attempt_id

    parent_final = task_store.get_task(parent_task_id)
    assert parent_final is not None
    assert parent_final["status"] == TaskCenterTaskStatus.FAILED.value


def test_delegated_retry_waits_until_final_graph(
    workflow_store, iteration_store, attempt_store, task_store, task_center_run_id, composer
) -> None:
    runtime = _build_runtime(
        workflow_store, iteration_store, attempt_store, task_store, composer=composer
    )
    parent_task_id, parent_attempt_id = _seed_outer_running_generator(
        runtime=runtime,
        workflow_store=workflow_store,
        iteration_store=iteration_store,
        attempt_store=attempt_store,
        task_store=task_store,
        task_center_run_id=task_center_run_id,
    )
    coordinator = WorkflowStarter(runtime=runtime)
    workflow_start = coordinator.start(
        prompt="delegated retry",
        origin=WorkflowOrigin.task(task_id=parent_task_id),
    )

    # Graph 1 fails — coordinator should retry inside same iteration, parent waits.
    _drive_delegated_attempt_to_fail(
        runtime=runtime, delegated_attempt_id=workflow_start.initial_attempt_id
    )
    segment1 = iteration_store.get(workflow_start.initial_iteration_id)
    assert segment1 is not None
    assert len(segment1.attempt_ids) == 2
    parent_mid = task_store.get_task(parent_task_id)
    assert parent_mid is not None
    assert parent_mid["status"] == TaskCenterTaskStatus.WAITING_WORKFLOW.value
    delegated_mid = workflow_store.get(workflow_start.workflow_id)
    assert delegated_mid is not None
    assert delegated_mid.status == WorkflowStatus.OPEN

    # Graph 2 passes terminally inside the same iteration — final close.
    retry_attempt_id = segment1.attempt_ids[1]
    _drive_delegated_attempt_to_pass(
        runtime=runtime,
        delegated_attempt_id=retry_attempt_id,
        deferred_goal_for_next_iteration=None,
    )

    parent_final = task_store.get_task(parent_task_id)
    delegated_final = workflow_store.get(workflow_start.workflow_id)
    refreshed_segment = iteration_store.get(workflow_start.initial_iteration_id)
    assert parent_final is not None
    assert parent_final["status"] == TaskCenterTaskStatus.DONE.value
    assert delegated_final is not None
    assert delegated_final.status == WorkflowStatus.SUCCEEDED
    assert refreshed_segment is not None
    assert refreshed_segment.status == IterationStatus.SUCCEEDED
