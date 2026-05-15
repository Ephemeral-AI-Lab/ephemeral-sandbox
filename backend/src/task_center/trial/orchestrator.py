"""TrialOrchestrator state machine."""

from __future__ import annotations

import logging
from collections.abc import Callable
from dataclasses import asdict
from datetime import UTC, datetime

from task_center.goal.state import GoalClosureReport
from task_center._core.types import TaskCenterInvariantViolation
from task_center.trial.dispatcher import AttemptDispatcher
from task_center.trial.state import (
    Trial,
    TrialFailReason,
    TrialStage,
    TrialStatus,
)
from task_center.trial.runtime import TrialDeps
from task_center.trial.launch import LaunchBuilder
from task_center._core.types import generator_task_id, planner_task_id
from task_center.task_state import (
    SpawnReason,
    EvaluatorSubmission,
    GeneratorSubmission,
    TaskCenterTaskRole,
    TaskCenterTaskStatus,
    PlannedGeneratorTask,
    PlannerFailureSubmission,
    PlannerSubmission,
)
from task_center.trial.generator_dag import (
    dependency_task_ids,
    ordered_generator_tasks,
)
from task_center._core.infra import (
    assert_evaluator_task_for_submission,
    assert_generator_task_for_submission,
    assert_trial_not_closed,
    assert_trial_stage,
    assert_task_belongs_to_trial,
    assert_valid_trial_close,
)

logger = logging.getLogger(__name__)


class TrialOrchestrator:
    """Runs one planner -> generator DAG -> evaluator harness trial."""

    def __init__(
        self,
        *,
        attempt: Trial,
        on_attempt_closed: Callable[[str], None],
        runtime: TrialDeps,
    ) -> None:
        self._attempt = attempt
        self._on_attempt_closed = on_attempt_closed
        self._runtime = runtime

        self._dispatcher = AttemptDispatcher(
            attempt_id=attempt.id,
            runtime=runtime,
            close_attempt=self._close_attempt,
        )

    @property
    def trial_id(self) -> str:
        return self._attempt.id

    def start(self) -> None:
        runtime = self._runtime
        attempt = self._assert_stage(TrialStage.PLAN)
        if attempt.status != TrialStatus.RUNNING:
            raise TaskCenterInvariantViolation(
                f"Trial {attempt.id!r} is not running"
            )
        if attempt.planner_task_id is not None:
            raise TaskCenterInvariantViolation(
                f"Trial {attempt.id!r} already has a planner task"
            )

        task_id = planner_task_id(attempt.id)
        runtime.orchestrator_registry.register(self)
        try:
            launch = LaunchBuilder(runtime=runtime).for_planner(
                attempt=attempt, task_id=task_id
            )
            runtime.task_store.upsert_task(
                task_id=task_id,
                task_center_run_id=launch.task_center_run_id,
                role=TaskCenterTaskRole.PLANNER.value,
                agent_name=launch.agent_name,
                rendered_prompt=launch.rendered_prompt,
                status=TaskCenterTaskStatus.RUNNING.value,
                summaries=[],
                needs=[],
                task_center_attempt_id=attempt.id,
                context_packet_id=launch.context_packet_id,
                spawn_reason=SpawnReason.TRIAL_PLANNER.value,
            )
            runtime.trial_store.set_planner_task_id(attempt.id, task_id)
            runtime.agent_launcher.launch(launch)
            self._dispatcher.dispatch_ready_work()
        except Exception:
            self._mark_startup_failed(planner_task_id=task_id)
            raise

    def apply_plan_submission(self, submission: PlannerSubmission) -> None:
        self._assert_submission_attempt(submission.attempt_id)
        if submission.kind == "full" and submission.continuation_goal is not None:
            raise TaskCenterInvariantViolation("Full plans cannot set continuation_goal")
        if submission.kind == "partial" and submission.continuation_goal is None:
            raise TaskCenterInvariantViolation("Partial plans require continuation_goal")

        attempt = self._validate_planner_submission(submission.planner_task_id)
        runtime = self._runtime
        runtime.task_store.set_task_status(
            submission.planner_task_id,
            status=TaskCenterTaskStatus.DONE.value,
            summary={"kind": submission.kind, "summary": submission.summary},
        )
        self._persist_plan_contract(submission)
        generator_ids = self._persist_generator_tasks(submission.tasks)
        runtime.trial_store.set_generator_task_ids(attempt.id, list(generator_ids))
        runtime.trial_store.set_stage(attempt.id, TrialStage.GENERATE)
        self._dispatcher.dispatch_ready_work()

    def apply_planner_failure(
        self, submission: PlannerFailureSubmission
    ) -> None:
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
        self._close_attempt(TrialStatus.FAILED, TrialFailReason.PLANNER_FAILED)

    def apply_generator_submission(
        self, submission: GeneratorSubmission
    ) -> None:
        self._assert_submission_attempt(submission.attempt_id)
        self._mark_generator(submission)
        if submission.outcome == "failure":
            self._dispatcher.block_failed_descendants(submission.task_id)
        self._dispatcher.dispatch_ready_work()

    def apply_evaluator_submission(
        self, submission: EvaluatorSubmission
    ) -> None:
        self._assert_submission_attempt(submission.attempt_id)
        self._mark_evaluator(submission)
        self._dispatcher.dispatch_ready_work()

    def apply_goal_closure_report(self, report: GoalClosureReport) -> None:
        """Resume a generator task waiting on a delegated goal.

        Idempotent: if the parent has already been resumed (status moved off
        ``waiting_mission`` by an earlier delivery), return silently
        without re-asserting trial stage or appending another summary.
        """
        runtime = self._runtime
        task = runtime.task_store.get_task(report.requested_by_task_id)
        if task is None:
            raise TaskCenterInvariantViolation(
                f"Generator task {report.requested_by_task_id!r} not found"
            )
        if task.get("status") != TaskCenterTaskStatus.WAITING_MISSION.value:
            # Already delivered; no further action.
            return

        attempt = self._assert_stage(TrialStage.GENERATE)
        assert_generator_task_for_submission(task, attempt)

        if report.outcome == "success":
            status = TaskCenterTaskStatus.DONE
            summary = (
                f"Delegated goal {report.goal_id} succeeded."
            )
        else:
            status = TaskCenterTaskStatus.FAILED
            summary = (
                f"Delegated goal {report.goal_id} failed."
            )

        updated = runtime.task_store.set_task_status_if_current(
            report.requested_by_task_id,
            expected_status=TaskCenterTaskStatus.WAITING_MISSION.value,
            status=status.value,
            summary={
                "outcome": report.outcome,
                "summary": summary,
                "payload": {
                    "goal_closure_report": asdict(report),
                    "submission_kind": "goal_closure_report",
                },
            },
        )
        if updated is None:
            # Race: another delivery moved the parent first. Idempotent.
            return
        if status == TaskCenterTaskStatus.FAILED:
            self._dispatcher.block_failed_descendants(report.requested_by_task_id)
        self._dispatcher.dispatch_ready_work()

    def _validate_planner_submission(self, planner_task_id: str) -> Trial:
        attempt = self._assert_stage(TrialStage.PLAN)
        if attempt.planner_task_id != planner_task_id:
            raise TaskCenterInvariantViolation(
                f"Planner submission task {planner_task_id!r} does not "
                f"match trial planner {attempt.planner_task_id!r}"
            )
        planner_task = self._runtime.task_store.get_task(planner_task_id)
        if planner_task is None:
            raise TaskCenterInvariantViolation(
                f"Planner task {planner_task_id!r} not found"
            )
        assert_task_belongs_to_trial(planner_task, attempt)
        if planner_task["role"] != TaskCenterTaskRole.PLANNER.value:
            raise TaskCenterInvariantViolation(
                f"Task {planner_task_id!r} is not a planner task"
            )
        return attempt

    def _persist_plan_contract(self, submission: PlannerSubmission) -> None:
        self._runtime.trial_store.set_plan_contract(
            submission.attempt_id,
            task_specification=submission.task_specification,
            evaluation_criteria=list(submission.evaluation_criteria),
            continuation_goal=submission.continuation_goal,
        )

    def _persist_generator_tasks(
        self, tasks: tuple[PlannedGeneratorTask, ...]
    ) -> tuple[str, ...]:
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
                rendered_prompt=task.task_spec,
                status=TaskCenterTaskStatus.PENDING.value,
                summaries=[],
                needs=list(needs),
                task_center_attempt_id=attempt.id,
                spawn_reason=SpawnReason.TRIAL_GENERATOR.value,
            )
            task_ids.append(task_id)
        return tuple(task_ids)

    def _mark_generator(self, submission: GeneratorSubmission) -> None:
        attempt = self._assert_stage(TrialStage.GENERATE)
        task = self._runtime.task_store.get_task(submission.task_id)
        if task is None:
            raise TaskCenterInvariantViolation(
                f"Generator task {submission.task_id!r} not found"
            )
        assert_generator_task_for_submission(task, attempt)
        self._write_submission_status(
            task=task, task_id=submission.task_id, role="Generator",
            outcome=submission.outcome, summary=submission.summary,
            payload=submission.payload,
        )

    def _mark_evaluator(self, submission: EvaluatorSubmission) -> None:
        attempt = self._assert_stage(TrialStage.EVALUATE)
        if attempt.evaluator_task_id != submission.task_id:
            raise TaskCenterInvariantViolation(
                f"Evaluator submission task {submission.task_id!r} does not "
                f"match trial evaluator {attempt.evaluator_task_id!r}"
            )
        task = self._runtime.task_store.get_task(submission.task_id)
        if task is None:
            raise TaskCenterInvariantViolation(
                f"Evaluator task {submission.task_id!r} not found"
            )
        assert_evaluator_task_for_submission(task, attempt)
        self._write_submission_status(
            task=task, task_id=submission.task_id, role="Evaluator",
            outcome=submission.outcome, summary=submission.summary,
            payload=submission.payload,
        )

    def _write_submission_status(
        self, *, task: dict, task_id: str, role: str,
        outcome: str, summary: str, payload: object,
    ) -> None:
        if task["status"] != TaskCenterTaskStatus.RUNNING.value:
            raise TaskCenterInvariantViolation(
                f"{role} task {task_id!r} is not running"
            )
        status = (
            TaskCenterTaskStatus.DONE
            if outcome == "success"
            else TaskCenterTaskStatus.FAILED
        )
        self._runtime.task_store.set_task_status(
            task_id,
            status=status.value,
            summary={"outcome": outcome, "summary": summary, "payload": payload},
        )

    def _close_attempt(
        self,
        status: TrialStatus,
        fail_reason: TrialFailReason | None,
    ) -> None:
        assert_valid_trial_close(status=status, fail_reason=fail_reason)
        attempt = self._fresh_attempt()
        assert_trial_not_closed(attempt)
        if attempt.status != TrialStatus.RUNNING:
            raise TaskCenterInvariantViolation(
                f"Trial {attempt.id!r} is not running"
            )
        self._runtime.trial_store.close(
            attempt.id,
            status=status,
            fail_reason=fail_reason,
            closed_at=datetime.now(UTC),
        )
        self._runtime.orchestrator_registry.deregister(attempt.id)
        self._on_attempt_closed(attempt.id)

    def _mark_startup_failed(self, *, planner_task_id: str) -> None:
        # Owns planner-task cleanup + registry deregistration. IterationManager's
        # _close_attempt_after_startup_failure (its catch in
        # _start_orchestrator_if_configured) owns the trial-close in both
        # paths — factory raises and start() raises.
        runtime = self._runtime
        runtime.orchestrator_registry.deregister(self._attempt.id)
        try:
            runtime.task_store.set_task_status_if_current(
                planner_task_id,
                expected_status=TaskCenterTaskStatus.RUNNING.value,
                status=TaskCenterTaskStatus.FAILED.value,
                summary={
                    "fail_reason": TrialFailReason.STARTUP_FAILED.value,
                },
            )
        except LookupError:
            pass
        except Exception:
            logger.exception(
                "TrialOrchestrator: startup task cleanup failed",
            )

    def _fresh_attempt(self) -> Trial:
        attempt = self._runtime.trial_store.get(self._attempt.id)
        if attempt is None:
            raise TaskCenterInvariantViolation(
                f"Trial {self._attempt.id!r} not found"
            )
        self._attempt = attempt
        return attempt

    def _assert_stage(self, expected: TrialStage) -> Trial:
        attempt = self._fresh_attempt()
        assert_trial_not_closed(attempt)
        assert_trial_stage(attempt, expected)
        return attempt

    def _assert_submission_attempt(self, attempt_id: str) -> None:
        if attempt_id != self._attempt.id:
            raise TaskCenterInvariantViolation(
                f"Submission attempt {attempt_id!r} does not match orchestrator "
                f"trial {self._attempt.id!r}"
            )
