"""AttemptOrchestrator state machine."""

from __future__ import annotations

import logging
from collections.abc import Callable
from dataclasses import asdict
from datetime import UTC, datetime
from typing import Any

from task_center._core.invariants import (
    assert_attempt_not_closed,
    assert_attempt_stage,
    assert_evaluator_task_for_submission,
    assert_generator_task_for_submission,
    assert_task_belongs_to_attempt,
    assert_valid_attempt_close,
)
from task_center._core.generator_summaries import (
    attempt_failure_line,
    child_outcomes_for_workflow,
    generator_outcomes,
    to_record,
)
from task_center._core.primitives import (
    TaskCenterInvariantViolation,
    generator_task_id,
    planner_task_id,
)
from task_center.attempt.stage_advancer import AttemptStageAdvancer
from task_center.attempt.generator_dag import (
    dependency_task_ids,
    ordered_generator_tasks,
)
from task_center.attempt.launch import AgentLaunchFactory
from task_center.attempt.deps import AttemptDeps
from task_center.attempt.state import (
    Attempt,
    AttemptFailReason,
    AttemptStage,
    AttemptStatus,
)
from task_center.workflow.state import WorkflowClosureReport
from task_center._core.task_state import (
    SpawnReason,
    TaskCenterTaskRole,
    TaskCenterTaskStatus,
)
from task_center.submissions import (
    EvaluatorSubmission,
    GeneratorSubmission,
    PlannedGeneratorTask,
    PlannerFailureSubmission,
    PlannerSubmission,
)

logger = logging.getLogger(__name__)


class AttemptOrchestrator:
    """Runs one planner -> generator DAG -> evaluator harness attempt."""

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
                summaries=[],
                needs=[],
                task_center_attempt_id=attempt.id,
                context_packet_id=launch.context_packet_id,
                spawn_reason=SpawnReason.ATTEMPT_PLANNER.value,
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
            summary={"kind": submission.kind, "summary": submission.summary},
        )
        self._persist_plan_contract(submission)
        generator_ids = self._persist_generator_tasks(submission.tasks)
        runtime.attempt_store.set_generator_task_ids(attempt.id, list(generator_ids))
        runtime.attempt_store.set_stage(attempt.id, AttemptStage.GENERATE)
        self._stage_advancer.advance_ready_tasks()

    def apply_planner_failure(self, submission: PlannerFailureSubmission) -> None:
        self._assert_submission_attempt(submission.attempt_id)
        self._validate_planner_submission(submission.planner_task_id)
        self._runtime.task_store.set_task_status(
            submission.planner_task_id,
            status=TaskCenterTaskStatus.FAILED.value,
            summary={
                "fail_reason": submission.fail_reason,
                "summary": submission.summary,
            },
        )
        self._close_attempt(AttemptStatus.FAILED, AttemptFailReason.PLANNER_FAILED)

    def apply_generator_submission(self, submission: GeneratorSubmission) -> None:
        self._assert_submission_attempt(submission.attempt_id)
        self._mark_generator(submission)
        self._stage_advancer.advance_ready_tasks()

    def apply_evaluator_submission(self, submission: EvaluatorSubmission) -> None:
        self._assert_submission_attempt(submission.attempt_id)
        self._mark_evaluator(submission)
        self._stage_advancer.advance_ready_tasks()

    def apply_workflow_closure_report(self, report: WorkflowClosureReport) -> None:
        """Resume a generator task waiting on a delegated workflow.

        Idempotent: if the parent has already been resumed (status moved off
        ``waiting_workflow`` by an earlier delivery), return silently
        without re-asserting attempt stage or appending another summary.
        """
        runtime = self._runtime
        parent_task_id = report.requested_by_task_id
        if parent_task_id is None:
            raise TaskCenterInvariantViolation(
                f"Workflow closure report {report.workflow_id!r} has no parent task id"
            )
        task = runtime.task_store.get_task(parent_task_id)
        if task is None:
            raise TaskCenterInvariantViolation(f"Generator task {parent_task_id!r} not found")
        if task.get("status") != TaskCenterTaskStatus.WAITING_WORKFLOW.value:
            # Already delivered; no further action.
            return

        attempt = self._assert_stage(AttemptStage.GENERATE)
        assert_generator_task_for_submission(task, attempt)

        if report.outcome == "success":
            status = TaskCenterTaskStatus.DONE
            summary = f"Delegated goal {report.workflow_id} succeeded."
        else:
            status = TaskCenterTaskStatus.FAILED
            summary = f"Delegated goal {report.workflow_id} failed."

        updated = runtime.task_store.set_task_status_if_current(
            parent_task_id,
            expected_status=TaskCenterTaskStatus.WAITING_WORKFLOW.value,
            status=status.value,
            summary={
                "outcome": report.outcome,
                "summary": summary,
                "payload": {
                    "workflow_closure_report": asdict(report),
                    "submission_kind": "workflow_closure_report",
                    "handoff_rollup": self._build_handoff_rollup(report),
                },
            },
        )
        if updated is None:
            # Race: another delivery moved the parent first. Idempotent.
            return
        self._stage_advancer.advance_ready_tasks()

    def _build_handoff_rollup(self, report: WorkflowClosureReport) -> dict[str, Any]:
        """Structured roll-up of the child goal, rendered later as nested ``<task>``.

        Success: the child generators across all SUCCEEDED child iterations.
        Failure: those, plus the final failed attempt's terminal generators and
        an ``attempt_failure_line`` as the ``<failure>`` child. The recipe layer
        turns this into nested ``<task>`` wherever the parent generator appears.
        """
        runtime = self._runtime
        children = [
            to_record(outcome)
            for outcome in child_outcomes_for_workflow(report.workflow_id, runtime.iteration_store)
        ]
        failure: str | None = None
        if report.outcome != "success" and report.final_attempt_id is not None:
            final_attempt = runtime.attempt_store.get(report.final_attempt_id)
            if final_attempt is not None:
                children.extend(
                    to_record(outcome)
                    for outcome in generator_outcomes(
                        final_attempt, task_store=runtime.task_store
                    )
                    if outcome.is_terminal
                )
                failure = attempt_failure_line(final_attempt, runtime.task_store)
        return {"children": children, "failure": failure}

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

    def _persist_plan_contract(self, submission: PlannerSubmission) -> None:
        self._runtime.attempt_store.set_plan_contract(
            submission.attempt_id,
            plan_spec=submission.plan_spec,
            evaluation_criteria=list(submission.evaluation_criteria),
            deferred_goal_for_next_iteration=submission.deferred_goal_for_next_iteration,
        )

    def _persist_generator_tasks(self, tasks: tuple[PlannedGeneratorTask, ...]) -> tuple[str, ...]:
        runtime = self._runtime
        attempt = self._fresh_attempt()
        ordered = ordered_generator_tasks(tasks)
        task_center_run_id = runtime.run_id_for_attempt(attempt)
        task_ids: list[str] = []
        for task in ordered:
            task_id = generator_task_id(attempt.id, task.local_id)
            needs = dependency_task_ids(
                attempt_id=attempt.id,
                local_deps=task.deps,
            )
            runtime.task_store.upsert_task(
                task_id=task_id,
                task_center_run_id=task_center_run_id,
                role=TaskCenterTaskRole.GENERATOR.value,
                agent_name=task.agent_name,
                context_message=task.task_spec,
                status=TaskCenterTaskStatus.PENDING.value,
                summaries=[],
                needs=list(needs),
                task_center_attempt_id=attempt.id,
                spawn_reason=SpawnReason.ATTEMPT_GENERATOR.value,
            )
            task_ids.append(task_id)
        return tuple(task_ids)

    def _mark_generator(self, submission: GeneratorSubmission) -> None:
        attempt = self._assert_stage(AttemptStage.GENERATE)
        task = self._runtime.task_store.get_task(submission.task_id)
        if task is None:
            raise TaskCenterInvariantViolation(f"Generator task {submission.task_id!r} not found")
        assert_generator_task_for_submission(task, attempt)
        self._write_submission_status(
            task=task,
            task_id=submission.task_id,
            role="Generator",
            outcome=submission.outcome,
            summary=submission.summary,
            payload=submission.payload,
        )

    def _mark_evaluator(self, submission: EvaluatorSubmission) -> None:
        attempt = self._assert_stage(AttemptStage.EVALUATE)
        if attempt.evaluator_task_id != submission.task_id:
            raise TaskCenterInvariantViolation(
                f"Evaluator submission task {submission.task_id!r} does not "
                f"match attempt evaluator {attempt.evaluator_task_id!r}"
            )
        task = self._runtime.task_store.get_task(submission.task_id)
        if task is None:
            raise TaskCenterInvariantViolation(f"Evaluator task {submission.task_id!r} not found")
        assert_evaluator_task_for_submission(task, attempt)
        self._write_submission_status(
            task=task,
            task_id=submission.task_id,
            role="Evaluator",
            outcome=submission.outcome,
            summary=submission.summary,
            payload=submission.payload,
        )

    def _write_submission_status(
        self,
        *,
        task: dict[str, Any],
        task_id: str,
        role: str,
        outcome: str,
        summary: str,
        payload: object,
    ) -> None:
        if task["status"] != TaskCenterTaskStatus.RUNNING.value:
            raise TaskCenterInvariantViolation(f"{role} task {task_id!r} is not running")
        if outcome == "success":
            status = TaskCenterTaskStatus.DONE
        elif outcome == "blocker":
            status = TaskCenterTaskStatus.BLOCKED
        else:
            status = TaskCenterTaskStatus.FAILED
        self._runtime.task_store.set_task_status(
            task_id,
            status=status.value,
            summary={"outcome": outcome, "summary": summary, "payload": payload},
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
                summary={
                    "fail_reason": AttemptFailReason.STARTUP_FAILED.value,
                },
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
