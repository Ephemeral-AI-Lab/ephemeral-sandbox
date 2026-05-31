"""AttemptOrchestrator state machine."""

from __future__ import annotations

import logging
from collections.abc import Callable
from datetime import UTC, datetime
from typing import Any

from task_center._core.invariants import (
    assert_attempt_not_closed,
    assert_attempt_stage,
    assert_generator_task_for_submission,
    assert_reducer_task_for_submission,
    assert_task_belongs_to_attempt,
    assert_valid_attempt_close,
)
from task_center._core.outcomes import (
    execution_outcome_for_submission,
    planner_outcome_from_submission,
    project_attempt_outcomes,
    to_record,
    workflow_outcomes,
)
from task_center._core.primitives import (
    TaskCenterInvariantViolation,
    generator_task_id,
    planner_task_id,
    reducer_task_id,
)
from task_center._core.state import (
    Attempt,
    AttemptFailReason,
    AttemptStage,
    AttemptStatus,
    Workflow,
    WorkflowStatus,
)
from task_center._core.task_state import (
    TaskCenterTaskRole,
    TaskCenterTaskStatus,
)
from task_center.attempt.launch import REDUCER_AGENT_NAME, AgentLaunchFactory, AttemptDeps
from task_center.attempt.plan_dag import ordered_plan_tasks
from task_center.attempt.run_stage import AttemptStageAdvancer
from task_center.submissions import (
    GeneratorSubmission,
    PlannedGeneratorTask,
    PlannedReducerTask,
    PlannerFailureSubmission,
    PlannerSubmission,
    ReducerSubmission,
)

logger = logging.getLogger(__name__)


class AttemptOrchestrator:
    """Runs one planner -> plan-DAG (generators + reducers) harness attempt."""

    def __init__(
        self,
        *,
        attempt: Attempt,
        on_attempt_closed: Callable[[str], None],
        runtime: AttemptDeps,
    ) -> None:
        self._attempt = attempt
        self._on_attempt_closed = on_attempt_closed
        self._runtime = runtime

        self._stage_advancer = AttemptStageAdvancer(
            attempt_id=attempt.id,
            runtime=runtime,
            close_attempt=self._close_attempt,
        )

    @property
    def attempt_id(self) -> str:
        return self._attempt.id

    def start(self) -> None:
        runtime = self._runtime
        attempt = self._assert_stage(AttemptStage.PLAN)
        if attempt.status != AttemptStatus.RUNNING:
            raise TaskCenterInvariantViolation(f"Attempt {attempt.id!r} is not running")
        if attempt.planner_task_id is not None:
            raise TaskCenterInvariantViolation(f"Attempt {attempt.id!r} already has a planner task")

        task_id = planner_task_id(attempt.id)
        runtime.orchestrator_registry.register(self)
        try:
            launch = AgentLaunchFactory(runtime=runtime).for_planner(attempt=attempt, task_id=task_id)
            runtime.task_store.upsert_task(
                task_id=task_id,
                task_center_run_id=launch.task_center_run_id,
                role=TaskCenterTaskRole.PLANNER.value,
                agent_name=launch.agent_name,
                context_message=launch.context,
                status=TaskCenterTaskStatus.RUNNING.value,
                outcomes=[],
                needs=[],
            )
            runtime.attempt_store.set_planner_task_id(attempt.id, task_id)
            runtime.agent_launcher.launch(launch)
            self._stage_advancer.advance_ready_tasks()
        except Exception:
            self._mark_startup_failed(planner_task_id=task_id)
            raise

    def apply_plan_submission(self, submission: PlannerSubmission) -> None:
        self._assert_submission_attempt(submission.attempt_id)
        if (
            submission.kind == "completes"
            and submission.deferred_goal_for_next_iteration is not None
        ):
            raise TaskCenterInvariantViolation(
                "Full plans cannot set deferred_goal_for_next_iteration"
            )
        if submission.kind == "defers" and submission.deferred_goal_for_next_iteration is None:
            raise TaskCenterInvariantViolation(
                "Partial plans require deferred_goal_for_next_iteration"
            )

        attempt = self._validate_planner_submission(submission.planner_task_id)
        runtime = self._runtime
        runtime.task_store.set_task_status(
            submission.planner_task_id,
            status=TaskCenterTaskStatus.DONE.value,
            outcomes=[to_record(planner_outcome_from_submission(submission))],
            terminal_tool_result={"kind": submission.kind},
        )
        runtime.attempt_store.set_deferred_goal(
            attempt.id,
            deferred_goal_for_next_iteration=submission.deferred_goal_for_next_iteration,
        )
        generator_ids, reducer_ids = self._persist_plan_tasks(
            submission.generators, submission.reducers
        )
        runtime.attempt_store.set_generator_task_ids(attempt.id, list(generator_ids))
        runtime.attempt_store.set_reducer_task_ids(attempt.id, list(reducer_ids))
        runtime.attempt_store.set_stage(attempt.id, AttemptStage.RUN)
        self._stage_advancer.advance_ready_tasks()

    def apply_planner_failure(self, submission: PlannerFailureSubmission) -> None:
        self._assert_submission_attempt(submission.attempt_id)
        self._validate_planner_submission(submission.planner_task_id)
        self._runtime.task_store.set_task_status(
            submission.planner_task_id,
            status=TaskCenterTaskStatus.FAILED.value,
            outcomes=[],
            terminal_tool_result={"fail_reason": submission.fail_reason},
        )
        self._close_attempt(AttemptStatus.FAILED, AttemptFailReason.TASK_FAILED)

    def apply_generator_submission(self, submission: GeneratorSubmission) -> None:
        self._assert_submission_attempt(submission.attempt_id)
        self._mark_generator(submission)
        self._stage_advancer.advance_ready_tasks()

    def apply_reducer_submission(self, submission: ReducerSubmission) -> None:
        self._assert_submission_attempt(submission.attempt_id)
        self._mark_reducer(submission)
        self._stage_advancer.advance_ready_tasks()

    # ---- child-workflow handoff -----------------------------------------

    def start_child_workflow(self, *, generator_task: dict[str, Any], child_workflow: Workflow) -> None:
        """Atomically flip the spawning generator RUNNING -> WAITING_WORKFLOW + link."""
        task_id = str(generator_task["task_id"])
        updated = self._runtime.task_store.set_task_status_if_current(
            task_id,
            expected_status=TaskCenterTaskStatus.RUNNING.value,
            status=TaskCenterTaskStatus.WAITING_WORKFLOW.value,
            child_workflow_id=child_workflow.id,
        )
        if updated is None:
            raise TaskCenterInvariantViolation(
                f"TaskCenter task {task_id!r} was not running when the delegated "
                "workflow start tried to mark it waiting."
            )

    def apply_child_workflow_outcome(
        self, *, generator_task: dict[str, Any], child_workflow: Workflow, final_attempt_id: str | None
    ) -> None:
        """Resolve a generator waiting on a child workflow.

        Idempotent: if the parent has already moved off ``waiting_workflow``
        (an earlier delivery / race), return silently. The parent task receives
        the child workflow's flattened execution outcomes directly.
        """
        runtime = self._runtime
        task_id = str(generator_task["task_id"])
        task = runtime.task_store.get_task(task_id)
        if task is None:
            raise TaskCenterInvariantViolation(f"Generator task {task_id!r} not found")
        if task.get("status") != TaskCenterTaskStatus.WAITING_WORKFLOW.value:
            return

        attempt = self._assert_stage(AttemptStage.RUN)
        assert_generator_task_for_submission(task, attempt)

        succeeded = child_workflow.status == WorkflowStatus.SUCCEEDED
        outcomes = tuple(
            workflow_outcomes(child_workflow, iteration_store=runtime.iteration_store)
        )
        status = TaskCenterTaskStatus.DONE if succeeded else TaskCenterTaskStatus.FAILED
        updated = runtime.task_store.set_task_status_if_current(
            task_id,
            expected_status=TaskCenterTaskStatus.WAITING_WORKFLOW.value,
            status=status.value,
            outcomes=[to_record(outcome) for outcome in outcomes],
            terminal_tool_result={"child_workflow_id": child_workflow.id},
        )
        if updated is None:
            return
        self._stage_advancer.advance_ready_tasks()

    def cancel_child_workflow(self, *, generator_task: dict[str, Any]) -> None:
        """Restore a generator to RUNNING after a failed child-workflow start."""
        self._runtime.task_store.set_task_status_if_current(
            str(generator_task["task_id"]),
            expected_status=TaskCenterTaskStatus.WAITING_WORKFLOW.value,
            status=TaskCenterTaskStatus.RUNNING.value,
        )

    # ---- internals ------------------------------------------------------

    def _validate_planner_submission(self, planner_task_id: str) -> Attempt:
        attempt = self._assert_stage(AttemptStage.PLAN)
        if attempt.planner_task_id != planner_task_id:
            raise TaskCenterInvariantViolation(
                f"Planner submission task {planner_task_id!r} does not "
                f"match attempt planner {attempt.planner_task_id!r}"
            )
        planner_task = self._runtime.task_store.get_task(planner_task_id)
        if planner_task is None:
            raise TaskCenterInvariantViolation(f"Planner task {planner_task_id!r} not found")
        assert_task_belongs_to_attempt(planner_task, attempt)
        if planner_task["role"] != TaskCenterTaskRole.PLANNER.value:
            raise TaskCenterInvariantViolation(f"Task {planner_task_id!r} is not a planner task")
        return attempt

    def _persist_plan_tasks(
        self,
        generators: tuple[PlannedGeneratorTask, ...],
        reducers: tuple[PlannedReducerTask, ...],
    ) -> tuple[tuple[str, ...], tuple[str, ...]]:
        runtime = self._runtime
        attempt = self._fresh_attempt()
        ordered_gen, ordered_red = ordered_plan_tasks(generators, reducers)
        run_id = runtime.run_id_for_attempt(attempt)
        id_map = {t.local_id: generator_task_id(attempt.id, t.local_id) for t in ordered_gen}
        id_map.update({r.local_id: reducer_task_id(attempt.id, r.local_id) for r in ordered_red})

        generator_ids: list[str] = []
        for task in ordered_gen:
            task_id = id_map[task.local_id]
            runtime.task_store.upsert_task(
                task_id=task_id,
                task_center_run_id=run_id,
                role=TaskCenterTaskRole.GENERATOR.value,
                agent_name=task.agent_name,
                context_message=task.task_spec,
                status=TaskCenterTaskStatus.PENDING.value,
                outcomes=[],
                needs=[id_map[dep] for dep in task.needs],
            )
            generator_ids.append(task_id)

        reducer_ids: list[str] = []
        for reducer in ordered_red:
            task_id = id_map[reducer.local_id]
            runtime.task_store.upsert_task(
                task_id=task_id,
                task_center_run_id=run_id,
                role=TaskCenterTaskRole.REDUCER.value,
                agent_name=REDUCER_AGENT_NAME,
                context_message=reducer.prompt,
                status=TaskCenterTaskStatus.PENDING.value,
                outcomes=[],
                needs=[id_map[dep] for dep in reducer.needs],
            )
            reducer_ids.append(task_id)
        return tuple(generator_ids), tuple(reducer_ids)

    def _mark_generator(self, submission: GeneratorSubmission) -> None:
        attempt = self._assert_stage(AttemptStage.RUN)
        task = self._runtime.task_store.get_task(submission.task_id)
        if task is None:
            raise TaskCenterInvariantViolation(f"Generator task {submission.task_id!r} not found")
        assert_generator_task_for_submission(task, attempt)
        self._write_submission_status(
            task=task,
            task_id=submission.task_id,
            role="Generator",
            status=submission.status,
            outcome=submission.outcome,
            terminal_tool_result=submission.terminal_tool_result,
        )

    def _mark_reducer(self, submission: ReducerSubmission) -> None:
        attempt = self._assert_stage(AttemptStage.RUN)
        if submission.task_id not in attempt.reducer_task_ids:
            raise TaskCenterInvariantViolation(
                f"Reducer submission task {submission.task_id!r} is not a "
                f"reducer of attempt {attempt.id!r}"
            )
        task = self._runtime.task_store.get_task(submission.task_id)
        if task is None:
            raise TaskCenterInvariantViolation(f"Reducer task {submission.task_id!r} not found")
        assert_reducer_task_for_submission(task, attempt)
        self._write_submission_status(
            task=task,
            task_id=submission.task_id,
            role="Reducer",
            status=submission.status,
            outcome=submission.outcome,
            terminal_tool_result=submission.terminal_tool_result,
        )

    def _write_submission_status(
        self,
        *,
        task: dict[str, Any],
        task_id: str,
        role: str,
        status: str,
        outcome: str,
        terminal_tool_result: dict[str, Any],
    ) -> None:
        if task["status"] != TaskCenterTaskStatus.RUNNING.value:
            raise TaskCenterInvariantViolation(f"{role} task {task_id!r} is not running")
        if status == "success":
            task_status = TaskCenterTaskStatus.DONE
        elif status == "failed":
            task_status = TaskCenterTaskStatus.FAILED
        else:
            task_status = TaskCenterTaskStatus.FAILED
        execution_status = "success" if task_status == TaskCenterTaskStatus.DONE else "failed"
        result = execution_outcome_for_submission(
            task_id=task_id,
            role="generator" if role == "Generator" else "reducer",
            status=execution_status,
            outcome=outcome,
        )
        self._runtime.task_store.set_task_status(
            task_id,
            status=task_status.value,
            outcomes=[to_record(result)],
            terminal_tool_result=terminal_tool_result,
        )

    def _close_attempt(
        self,
        status: AttemptStatus,
        fail_reason: AttemptFailReason | None,
    ) -> None:
        assert_valid_attempt_close(status=status, fail_reason=fail_reason)
        attempt = self._fresh_attempt()
        assert_attempt_not_closed(attempt)
        if attempt.status != AttemptStatus.RUNNING:
            raise TaskCenterInvariantViolation(f"Attempt {attempt.id!r} is not running")
        self._runtime.attempt_store.close(
            attempt.id,
            status=status,
            fail_reason=fail_reason,
            outcomes=[
                to_record(outcome)
                for outcome in project_attempt_outcomes(attempt, self._runtime.task_store)
            ],
            closed_at=datetime.now(UTC),
        )
        self._runtime.orchestrator_registry.deregister(attempt.id)
        self._on_attempt_closed(attempt.id)

    def _mark_startup_failed(self, *, planner_task_id: str) -> None:
        # Owns planner-task cleanup + registry deregistration. IterationAttemptCoordinator's
        # _close_attempt_after_startup_failure (its catch in
        # _start_orchestrator_if_configured) owns the attempt-close in both
        # paths — factory raises and start() raises.
        runtime = self._runtime
        runtime.orchestrator_registry.deregister(self._attempt.id)
        try:
            runtime.task_store.set_task_status_if_current(
                planner_task_id,
                expected_status=TaskCenterTaskStatus.RUNNING.value,
                status=TaskCenterTaskStatus.FAILED.value,
                outcomes=[],
                terminal_tool_result={"fail_reason": AttemptFailReason.STARTUP_FAILED.value},
            )
        except LookupError:
            pass
        except Exception:
            logger.exception(
                "AttemptOrchestrator: startup task cleanup failed",
            )

    def _fresh_attempt(self) -> Attempt:
        attempt = self._runtime.attempt_store.get(self._attempt.id)
        if attempt is None:
            raise TaskCenterInvariantViolation(f"Attempt {self._attempt.id!r} not found")
        self._attempt = attempt
        return attempt

    def _assert_stage(self, expected: AttemptStage) -> Attempt:
        attempt = self._fresh_attempt()
        assert_attempt_not_closed(attempt)
        assert_attempt_stage(attempt, expected)
        return attempt

    def _assert_submission_attempt(self, attempt_id: str) -> None:
        if attempt_id != self._attempt.id:
            raise TaskCenterInvariantViolation(
                f"Submission attempt {attempt_id!r} does not match orchestrator "
                f"attempt {self._attempt.id!r}"
            )
