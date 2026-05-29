"""Iteration attempt coordination and process-local registry.

``IterationAttemptCoordinator`` is the sole creator of attempt records inside
its owned iteration and the only emitter of ``IterationClosureReport``.
``OpenIterationCoordinatorRegistry`` is the process-local one-coordinator-per-
open-iteration registry.
"""

from __future__ import annotations

import json
import logging
from collections.abc import Callable
from datetime import UTC, datetime

from task_center._core.generator_summaries import generator_outcomes, to_record
from task_center._core.invariants import (
    assert_attempt_belongs_to_iteration,
    assert_attempt_sequence_contiguous,
    assert_fail_reason_present_on_failure,
    assert_iteration_has_budget,
    assert_iteration_open,
)
from task_center._core.persistence import (
    AttemptStoreProtocol,
    IterationStoreProtocol,
    TaskStoreProtocol,
)
from task_center._core.primitives import TaskCenterInvariantViolation
from task_center.attempt.orchestrator_registry import RegisteredAttemptOrchestrator
from task_center.attempt.state import (
    Attempt,
    AttemptFailReason,
    AttemptStatus,
)
from task_center.iteration.state import (
    FailedAttemptEntry,
    AttemptPlanFailed,
    Iteration,
    IterationClosureReport,
    IterationStatus,
    SuccessDeferred,
    TerminalSuccess,
)

logger = logging.getLogger(__name__)


IterationClosureCallback = Callable[[IterationClosureReport], None]
AttemptClosedCallback = Callable[[str], None]
OrchestratorFactory = Callable[[Attempt, AttemptClosedCallback], RegisteredAttemptOrchestrator]


class IterationAttemptCoordinator:
    """Coordinates attempts for one open Iteration."""

    def __init__(
        self,
        *,
        iteration_id: str,
        iteration_store: IterationStoreProtocol,
        attempt_store: AttemptStoreProtocol,
        on_iteration_closed: IterationClosureCallback,
        orchestrator_factory: OrchestratorFactory | None = None,
        task_store: TaskStoreProtocol | None = None,
    ) -> None:
        self.iteration_id = iteration_id
        self._iteration_store = iteration_store
        self._attempt_store = attempt_store
        self._on_iteration_closed = on_iteration_closed
        self._orchestrator_factory = orchestrator_factory
        # Optional: when present, the coordinator denormalizes the passing
        # attempt's generators onto the iteration row (a structured achieved
        # record) at successful close so the context engine's planner recipe
        # can render them as ``<task>`` children on retry / chain.
        self._task_store = task_store

    # ---- public API -----------------------------------------------------

    def create_initial_attempt(self) -> Attempt:
        """Create attempt sequence 1 and start its orchestrator."""
        attempt = self.create_unstarted_initial_attempt()
        self.start_attempt(attempt)
        return attempt

    def create_unstarted_initial_attempt(self) -> Attempt:
        """Create attempt sequence 1 without starting its orchestrator."""
        iteration = self._current_iteration_snapshot()
        assert_iteration_open(iteration)
        if iteration.attempt_ids:
            raise TaskCenterInvariantViolation(
                f"Iteration {iteration.id!r} already has attempts; use create_next_attempt"
            )
        return self._insert_attempt(iteration, attempt_sequence_no=1)

    def start_attempt(self, attempt: Attempt) -> None:
        """Start an attempt that belongs to this coordinator's open iteration."""
        iteration = self._current_iteration_snapshot()
        assert_iteration_open(iteration)
        assert_attempt_belongs_to_iteration(attempt, iteration)
        self._start_orchestrator_if_configured(attempt)

    def create_next_attempt(self, *, previous_attempt_id: str) -> Attempt:
        """Called after a failed attempt if the iteration still has budget."""
        iteration = self._current_iteration_snapshot()
        assert_iteration_open(iteration)
        assert_iteration_has_budget(iteration)
        if iteration.latest_attempt_id != previous_attempt_id:
            raise TaskCenterInvariantViolation(
                f"previous_attempt_id {previous_attempt_id!r} is not "
                f"the latest attempt of iteration {iteration.id!r} "
                f"(latest={iteration.latest_attempt_id!r})"
            )
        attempt = self._insert_attempt(iteration, attempt_sequence_no=iteration.attempt_count + 1)
        self._start_orchestrator_if_configured(attempt)
        return attempt

    def handle_attempt_closed(self, attempt_id: str) -> None:
        """Entry point for the closed-attempt callback from the orchestrator."""
        attempt = self._attempt_store.get(attempt_id)
        if attempt is None:
            raise TaskCenterInvariantViolation(f"Attempt {attempt_id!r} not found")
        iteration = self._current_iteration_snapshot()
        assert_iteration_open(iteration)
        assert_attempt_belongs_to_iteration(attempt, iteration)
        assert_fail_reason_present_on_failure(attempt)

        if attempt.status == AttemptStatus.PASSED:
            self._close_iteration_passed(attempt)
        else:
            self._retry_or_close_failed(attempt)

    # ---- internal -------------------------------------------------------

    def _current_iteration_snapshot(self) -> Iteration:
        iteration = self._iteration_store.get(self.iteration_id)
        if iteration is None:
            raise TaskCenterInvariantViolation(f"Iteration {self.iteration_id!r} not found")
        return iteration

    def _insert_attempt(self, iteration: Iteration, *, attempt_sequence_no: int) -> Attempt:
        assert_attempt_sequence_contiguous(iteration, attempt_sequence_no)
        attempt = self._attempt_store.insert(
            iteration_id=iteration.id,
            attempt_sequence_no=attempt_sequence_no,
        )
        self._iteration_store.append_attempt_id(iteration.id, attempt.id)
        return attempt

    def _start_orchestrator_if_configured(self, attempt: Attempt) -> None:
        if self._orchestrator_factory is None:
            return
        try:
            orchestrator = self._orchestrator_factory(attempt, self.handle_attempt_closed)
            orchestrator.start()
        except Exception:
            self._close_attempt_after_startup_failure(attempt)
            raise

    def _close_attempt_after_startup_failure(self, attempt: Attempt) -> None:
        try:
            latest = self._attempt_store.get(attempt.id)
            if latest is None or latest.is_closed:
                return
            self._attempt_store.close(
                attempt.id,
                status=AttemptStatus.FAILED,
                fail_reason=AttemptFailReason.STARTUP_FAILED,
                closed_at=datetime.now(UTC),
            )
        except Exception:
            logger.exception(
                "IterationAttemptCoordinator: startup attempt cleanup failed",
            )

    def _close_iteration_passed(self, attempt: Attempt) -> None:
        self._iteration_store.set_deferred_goal_for_next_iteration(
            self.iteration_id,
            deferred_goal_for_next_iteration=attempt.deferred_goal_for_next_iteration,
        )
        # Atomically transition status + write the denormalized plan_spec (from
        # the passing attempt) and the structured achieved record (a JSON list
        # of ``{local_id, status, summary}`` over the passing generators) onto
        # the iteration row. The planner recipe reads the achieved record; it
        # no longer surfaces plan_spec.
        self._iteration_store.close_succeeded(
            self.iteration_id,
            plan_spec=attempt.plan_spec or "",
            task_summary=self._achieved_record_for(attempt),
            closed_at=datetime.now(UTC),
        )
        if attempt.deferred_goal_for_next_iteration is None:
            self._emit_terminal_success(attempt)
        else:
            self._emit_success_deferred(attempt)

    def _achieved_record_for(self, attempt: Attempt) -> str:
        """JSON achieved record for the passing attempt's generators.

        Empty list (``"[]"``) when the coordinator is configured without a
        ``task_store`` (test seams). All generators of a passing attempt are
        ``DONE``, so each entry's status maps to ``success``.
        """
        outcomes = generator_outcomes(attempt, task_store=self._task_store)
        return json.dumps([to_record(o) for o in outcomes])

    def _retry_or_close_failed(self, attempt: Attempt) -> None:
        while True:
            iteration = self._current_iteration_snapshot()
            if not iteration.has_budget_remaining:
                self._close_iteration_failed(attempt)
                return
            try:
                self.create_next_attempt(previous_attempt_id=attempt.id)
                return
            except Exception:
                # Retry start failed; the new attempt was inserted and closed
                # STARTUP_FAILED before the exception propagated. Re-enter the
                # retry decision on the new closed attempt instead of leaving
                # the iteration open.
                retry_attempt = self._latest_failed_attempt_for(previous_id=attempt.id)
                if retry_attempt is None:
                    raise
                logger.warning(
                    "IterationAttemptCoordinator: retry start failure for iteration %r; "
                    "treating new attempt %r as a failed attempt",
                    self.iteration_id,
                    retry_attempt.id,
                    exc_info=True,
                )
                attempt = retry_attempt
                continue

    def _close_iteration_failed(self, attempt: Attempt) -> None:
        self._iteration_store.set_status(
            self.iteration_id,
            status=IterationStatus.FAILED,
            closed_at=datetime.now(UTC),
        )
        self._emit_attempt_plan_failed(attempt)

    def _latest_failed_attempt_for(self, *, previous_id: str) -> Attempt | None:
        iteration = self._current_iteration_snapshot()
        latest_id = iteration.latest_attempt_id
        if latest_id is None or latest_id == previous_id:
            return None
        retry_attempt = self._attempt_store.get(latest_id)
        if retry_attempt is None or retry_attempt.status != AttemptStatus.FAILED:
            return None
        return retry_attempt

    def _emit_terminal_success(self, attempt: Attempt) -> None:
        report = IterationClosureReport(
            iteration_id=self.iteration_id,
            final_attempt_id=attempt.id,
            outcome=TerminalSuccess(),
        )
        self._on_iteration_closed(report)

    def _emit_success_deferred(self, attempt: Attempt) -> None:
        if attempt.deferred_goal_for_next_iteration is None:
            raise TaskCenterInvariantViolation(
                "success_deferred requires a non-null deferred_goal_for_next_iteration"
            )
        report = IterationClosureReport(
            iteration_id=self.iteration_id,
            final_attempt_id=attempt.id,
            outcome=SuccessDeferred(
                deferred_goal_for_next_iteration=attempt.deferred_goal_for_next_iteration
            ),
        )
        self._on_iteration_closed(report)

    def _emit_attempt_plan_failed(self, last_attempt: Attempt) -> None:
        history = self._build_prior_attempt_history()
        report = IterationClosureReport(
            iteration_id=self.iteration_id,
            final_attempt_id=last_attempt.id,
            outcome=AttemptPlanFailed(
                failure_summary=(
                    last_attempt.fail_reason.value
                    if last_attempt.fail_reason is not None
                    else "unknown"
                ),
                prior_attempt_history=history,
            ),
        )
        self._on_iteration_closed(report)

    def _build_prior_attempt_history(self) -> tuple[FailedAttemptEntry, ...]:
        attempts = self._attempt_store.list_for_iteration(self.iteration_id)
        return tuple(
            FailedAttemptEntry(
                attempt_id=g.id,
                attempt_sequence_no=g.attempt_sequence_no,
                plan_spec=g.plan_spec,
                evaluation_criteria=g.evaluation_criteria,
                fail_reason=g.fail_reason,
            )
            for g in attempts
        )


class OpenIterationCoordinatorRegistry:
    """In-memory registry enforcing one coordinator per open iteration."""

    def __init__(self) -> None:
        self._by_iteration_id: dict[str, IterationAttemptCoordinator] = {}

    def register(self, coordinator: IterationAttemptCoordinator) -> None:
        iteration_id = coordinator.iteration_id
        if iteration_id in self._by_iteration_id:
            raise TaskCenterInvariantViolation(
                f"IterationAttemptCoordinator already registered for iteration {iteration_id!r}"
            )
        self._by_iteration_id[iteration_id] = coordinator

    def get(self, iteration_id: str) -> IterationAttemptCoordinator | None:
        return self._by_iteration_id.get(iteration_id)

    def deregister(self, iteration_id: str) -> None:
        self._by_iteration_id.pop(iteration_id, None)


__all__ = [
    "AttemptClosedCallback",
    "IterationClosureCallback",
    "IterationAttemptCoordinator",
    "OpenIterationCoordinatorRegistry",
    "OrchestratorFactory",
]
