"""Iteration lifecycle manager and process-local registry.

``IterationManager`` is the sole creator of Trial records inside its owned
iteration and the only emitter of ``IterationClosureReport``.
``IterationManagerRegistry`` is the process-local one-manager-per-open-iteration
registry.
"""

from __future__ import annotations

import logging
from collections.abc import Callable
from datetime import UTC, datetime

from task_center._core.infra import (
    assert_trial_belongs_to_iteration,
    assert_trial_sequence_contiguous,
    assert_iteration_has_budget,
    assert_iteration_open,
    assert_fail_reason_present_on_failure,
)
from task_center._core.persistence import (
    TrialStoreProtocol,
    IterationStoreProtocol,
    TaskStoreProtocol,
)
from task_center._core.types import (
    RegisteredTrialOrchestrator,
    TaskCenterInvariantViolation,
)
from task_center.trial.state import (
    Trial,
    TrialFailReason,
    TrialStatus,
)
from task_center.iteration.state import (
    PriorTrialEntry,
    TrialPlanFailed,
    Iteration,
    IterationClosureReport,
    IterationStatus,
    SuccessContinue,
    TerminalSuccess,
)

logger = logging.getLogger(__name__)


ClosureReportSink = Callable[[IterationClosureReport], None]
AttemptClosedCallback = Callable[[str], None]
OrchestratorFactory = Callable[
    [Trial, AttemptClosedCallback], RegisteredTrialOrchestrator
]


class IterationManager:
    """Manages one open Iteration's lifecycle."""

    def __init__(
        self,
        *,
        iteration_id: str,
        iteration_store: IterationStoreProtocol,
        trial_store: TrialStoreProtocol,
        on_episode_closed: ClosureReportSink,
        orchestrator_factory: OrchestratorFactory | None = None,
        task_store: TaskStoreProtocol | None = None,
    ) -> None:
        self.iteration_id = iteration_id
        self._iteration_store = iteration_store
        self._trial_store = trial_store
        self._on_episode_closed = on_episode_closed
        self._orchestrator_factory = orchestrator_factory
        # Optional — when present, the manager denormalizes the evaluator's
        # pass-summary text onto the iteration row at successful close so the
        # context engine's planner recipe can read it on retry / chain.
        self._task_store = task_store

    # ---- public API -----------------------------------------------------

    def create_initial_attempt(self) -> Trial:
        """Create trial sequence 1 and start its orchestrator."""
        attempt = self.create_unstarted_initial_attempt()
        self.start_attempt(attempt)
        return attempt

    def create_unstarted_initial_attempt(self) -> Trial:
        """Create trial sequence 1 without starting its orchestrator."""
        iteration = self._current_iteration_snapshot()
        assert_iteration_open(iteration)
        if iteration.trial_ids:
            raise TaskCenterInvariantViolation(
                f"Iteration {iteration.id!r} already has trials; use "
                f"create_next_attempt"
            )
        return self._insert_attempt(iteration, trial_sequence_no=1)

    def start_attempt(self, attempt: Trial) -> None:
        """Start a trial that belongs to this manager's open iteration."""
        iteration = self._current_iteration_snapshot()
        assert_iteration_open(iteration)
        assert_trial_belongs_to_iteration(attempt, iteration)
        self._start_orchestrator_if_configured(attempt)

    def create_next_attempt(
        self, *, previous_attempt_id: str
    ) -> Trial:
        """Called after a failed trial if the iteration still has budget."""
        iteration = self._current_iteration_snapshot()
        assert_iteration_open(iteration)
        assert_iteration_has_budget(iteration)
        if iteration.latest_trial_id != previous_attempt_id:
            raise TaskCenterInvariantViolation(
                f"previous_attempt_id {previous_attempt_id!r} is not "
                f"the latest trial of iteration {iteration.id!r} "
                f"(latest={iteration.latest_trial_id!r})"
            )
        attempt = self._insert_attempt(
            iteration, trial_sequence_no=iteration.trial_count + 1
        )
        self._start_orchestrator_if_configured(attempt)
        return attempt

    def handle_attempt_closed(self, attempt_id: str) -> None:
        """Entry point for the closed-trial callback from the orchestrator."""
        attempt = self._trial_store.get(attempt_id)
        if attempt is None:
            raise TaskCenterInvariantViolation(
                f"Trial {attempt_id!r} not found"
            )
        iteration = self._current_iteration_snapshot()
        assert_iteration_open(iteration)
        assert_trial_belongs_to_iteration(attempt, iteration)
        assert_fail_reason_present_on_failure(attempt)

        if attempt.status == TrialStatus.PASSED:
            self._close_iteration_passed(attempt)
        else:
            self._retry_or_close_failed(attempt)

    # ---- internal -------------------------------------------------------

    def _current_iteration_snapshot(self) -> Iteration:
        iteration = self._iteration_store.get(self.iteration_id)
        if iteration is None:
            raise TaskCenterInvariantViolation(
                f"Iteration {self.iteration_id!r} not found"
            )
        return iteration

    def _insert_attempt(
        self, iteration: Iteration, *, trial_sequence_no: int
    ) -> Trial:
        assert_trial_sequence_contiguous(iteration, trial_sequence_no)
        attempt = self._trial_store.insert(
            iteration_id=iteration.id,
            trial_sequence_no=trial_sequence_no,
        )
        self._iteration_store.append_trial_id(iteration.id, attempt.id)
        return attempt

    def _start_orchestrator_if_configured(self, attempt: Trial) -> None:
        if self._orchestrator_factory is None:
            return
        try:
            orchestrator = self._orchestrator_factory(
                attempt, self.handle_attempt_closed
            )
            orchestrator.start()
        except Exception:
            self._close_attempt_after_startup_failure(attempt)
            raise

    def _close_attempt_after_startup_failure(self, attempt: Trial) -> None:
        try:
            latest = self._trial_store.get(attempt.id)
            if latest is None or latest.is_closed:
                return
            self._trial_store.close(
                attempt.id,
                status=TrialStatus.FAILED,
                fail_reason=TrialFailReason.STARTUP_FAILED,
                closed_at=datetime.now(UTC),
            )
        except Exception:
            logger.exception(
                "IterationManager: startup trial cleanup failed",
            )

    def _close_iteration_passed(self, attempt: Trial) -> None:
        self._iteration_store.set_continuation_goal(
            self.iteration_id, attempt.continuation_goal
        )
        # Atomically transition status + write the denormalized
        # task_specification (from the passing trial) and task_summary
        # (from the evaluator's pass summary text) onto the iteration row.
        self._iteration_store.close_succeeded(
            self.iteration_id,
            task_specification=attempt.task_specification or "",
            task_summary=self._evaluator_pass_summary_for(attempt),
            closed_at=datetime.now(UTC),
        )
        if attempt.continuation_goal is None:
            self._emit_terminal_success(attempt)
        else:
            self._emit_success_continue(attempt)

    def _evaluator_pass_summary_for(self, attempt: Trial) -> str:
        """Resolve the evaluator's success-summary text for *attempt*.

        Empty string when the manager is configured without a ``task_store``
        (test seams) or when the evaluator never recorded a summary.
        """
        if self._task_store is None:
            return ""
        return self._task_store.get_evaluator_pass_summary(attempt.id)

    def _retry_or_close_failed(self, attempt: Trial) -> None:
        while True:
            iteration = self._current_iteration_snapshot()
            if not iteration.has_budget_remaining:
                self._close_iteration_failed(attempt)
                return
            try:
                self.create_next_attempt(previous_attempt_id=attempt.id)
                return
            except Exception:
                # Retry start failed; the new trial was inserted and closed
                # STARTUP_FAILED before the exception propagated. Re-enter the
                # retry decision on the new closed trial instead of leaving
                # the iteration open.
                retry_attempt = self._latest_failed_attempt_for(
                    previous_id=attempt.id
                )
                if retry_attempt is None:
                    raise
                logger.warning(
                    "IterationManager: retry start failure for iteration %r; "
                    "treating new trial %r as a failed trial",
                    self.iteration_id,
                    retry_attempt.id,
                    exc_info=True,
                )
                attempt = retry_attempt
                continue

    def _close_iteration_failed(self, attempt: Trial) -> None:
        self._iteration_store.set_status(
            self.iteration_id,
            status=IterationStatus.FAILED,
            closed_at=datetime.now(UTC),
        )
        self._emit_trial_plan_failed(attempt)

    def _latest_failed_attempt_for(
        self, *, previous_id: str
    ) -> Trial | None:
        iteration = self._current_iteration_snapshot()
        latest_id = iteration.latest_trial_id
        if latest_id is None or latest_id == previous_id:
            return None
        retry_attempt = self._trial_store.get(latest_id)
        if retry_attempt is None or retry_attempt.status != TrialStatus.FAILED:
            return None
        return retry_attempt

    def _emit_terminal_success(self, attempt: Trial) -> None:
        report = IterationClosureReport(
            iteration_id=self.iteration_id,
            final_trial_id=attempt.id,
            outcome=TerminalSuccess(),
        )
        self._on_episode_closed(report)

    def _emit_success_continue(self, attempt: Trial) -> None:
        if attempt.continuation_goal is None:
            raise TaskCenterInvariantViolation(
                "success_continue requires a non-null continuation_goal"
            )
        report = IterationClosureReport(
            iteration_id=self.iteration_id,
            final_trial_id=attempt.id,
            outcome=SuccessContinue(goal=attempt.continuation_goal),
        )
        self._on_episode_closed(report)

    def _emit_trial_plan_failed(self, last_attempt: Trial) -> None:
        history = self._build_prior_trial_history()
        report = IterationClosureReport(
            iteration_id=self.iteration_id,
            final_trial_id=last_attempt.id,
            outcome=TrialPlanFailed(
                failure_summary=(
                    last_attempt.fail_reason.value
                    if last_attempt.fail_reason is not None
                    else "unknown"
                ),
                prior_trial_history=history,
            ),
        )
        self._on_episode_closed(report)

    def _build_prior_trial_history(self) -> tuple[PriorTrialEntry, ...]:
        trials = self._trial_store.list_for_iteration(self.iteration_id)
        return tuple(
            PriorTrialEntry(
                trial_id=g.id,
                trial_sequence_no=g.trial_sequence_no,
                task_specification=g.task_specification,
                evaluation_criteria=g.evaluation_criteria,
                fail_reason=g.fail_reason,
                trial_summary_id=None,
                failure_landscape=None,
            )
            for g in trials
        )


class IterationManagerRegistry:
    """In-memory registry enforcing one-manager-per-open-iteration."""

    def __init__(self) -> None:
        self._by_iteration_id: dict[str, IterationManager] = {}

    def register(self, manager: IterationManager) -> None:
        iteration_id = manager.iteration_id
        if iteration_id in self._by_iteration_id:
            raise TaskCenterInvariantViolation(
                f"IterationManager already registered for iteration {iteration_id!r}"
            )
        self._by_iteration_id[iteration_id] = manager

    def get(self, iteration_id: str) -> IterationManager | None:
        return self._by_iteration_id.get(iteration_id)

    def deregister(self, iteration_id: str) -> None:
        self._by_iteration_id.pop(iteration_id, None)


__all__ = [
    "AttemptClosedCallback",
    "ClosureReportSink",
    "IterationManager",
    "IterationManagerRegistry",
    "OrchestratorFactory",
]
