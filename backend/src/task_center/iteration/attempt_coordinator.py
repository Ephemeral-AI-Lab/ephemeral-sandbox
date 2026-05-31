"""Iteration attempt coordination and process-local registry.

``IterationAttemptCoordinator`` is the sole creator of attempt records inside
its owned iteration and the only signaller of iteration close (a primitive
callback, not a report DTO). ``OpenIterationCoordinatorRegistry`` is the
process-local one-coordinator-per-open-iteration registry.
"""

from __future__ import annotations

import logging
from collections.abc import Callable
from datetime import UTC, datetime

from task_center._core.invariants import (
    assert_attempt_belongs_to_iteration,
    assert_attempt_sequence_contiguous,
    assert_fail_reason_present_on_failure,
    assert_iteration_has_budget,
    assert_iteration_open,
)
from task_center._core.outcomes import (
    project_iteration_outcomes,
    records_json,
)
from task_center._core.persistence import (
    AttemptStoreProtocol,
    IterationStoreProtocol,
    TaskStoreProtocol,
)
from task_center._core.primitives import TaskCenterInvariantViolation
from task_center._core.state import (
    Attempt,
    AttemptFailReason,
    AttemptStatus,
    Iteration,
    IterationStatus,
)
from task_center.attempt.orchestrator_registry import RegisteredAttemptOrchestrator

logger = logging.getLogger(__name__)


# Signalled on iteration close: (iteration_id, succeeded, deferred_goal,
# final_attempt_id). succeeded + deferred_goal=None -> workflow success;
# succeeded + deferred_goal -> next iteration; not succeeded -> workflow fail.
IterationClosedCallback = Callable[..., None]
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
        on_iteration_closed: IterationClosedCallback,
        orchestrator_factory: OrchestratorFactory | None = None,
        task_store: TaskStoreProtocol | None = None,
    ) -> None:
        self.iteration_id = iteration_id
        self._iteration_store = iteration_store
        self._attempt_store = attempt_store
        self._on_iteration_closed = on_iteration_closed
        self._orchestrator_factory = orchestrator_factory
        # Optional: when present, the coordinator denormalizes the canonical
        # outcomes (the passing attempt's reducer outcomes, or a failed
        # attempt's failed-task outcomes) onto the iteration row at close so the
        # planner recipe can relay them on the next iteration.
        self._task_store = task_store

    # ---- public API -----------------------------------------------------

    def create_attempt(
        self, *, previous_attempt_id: str | None = None, start: bool = True
    ) -> Attempt:
        """Create an attempt for this iteration.

        ``previous_attempt_id=None`` creates the iteration's first attempt
        (sequence 1, rejected if any attempt already exists). A
        ``previous_attempt_id`` makes it a retry: the iteration must still have
        budget and the id must be the current latest attempt. ``start=False``
        inserts the attempt without starting its orchestrator so the caller can
        start it later via :meth:`start_attempt`.
        """
        iteration = self._current_iteration_snapshot()
        assert_iteration_open(iteration)
        if previous_attempt_id is None:
            if iteration.attempt_ids:
                raise TaskCenterInvariantViolation(
                    f"Iteration {iteration.id!r} already has attempts; "
                    "pass previous_attempt_id to retry"
                )
            sequence_no = 1
        else:
            assert_iteration_has_budget(iteration)
            if iteration.latest_attempt_id != previous_attempt_id:
                raise TaskCenterInvariantViolation(
                    f"previous_attempt_id {previous_attempt_id!r} is not "
                    f"the latest attempt of iteration {iteration.id!r} "
                    f"(latest={iteration.latest_attempt_id!r})"
                )
            sequence_no = iteration.attempt_count + 1
        attempt = self._insert_attempt(iteration, attempt_sequence_no=sequence_no)
        if start:
            self._start_orchestrator_if_configured(attempt)
        return attempt

    def create_and_start_first_attempt(
        self, *, before_start: Callable[[Attempt], None] | None = None
    ) -> Attempt:
        """Create the iteration's first attempt and start it.

        ``before_start`` runs after the attempt row exists but before its
        orchestrator starts, so the entry path can mark the parent task waiting
        between create and start.
        """
        attempt = self.create_attempt(start=False)
        if before_start is not None:
            before_start(attempt)
        self.start_attempt(attempt)
        return attempt

    def start_attempt(self, attempt: Attempt) -> None:
        """Start an attempt that belongs to this coordinator's open iteration."""
        iteration = self._current_iteration_snapshot()
        assert_iteration_open(iteration)
        assert_attempt_belongs_to_iteration(attempt, iteration)
        self._start_orchestrator_if_configured(attempt)

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
        # Atomically transition to SUCCEEDED + write the canonical outcomes
        # (the passing attempt's reducer outcomes). The planner recipe relays
        # them on the next iteration.
        self._iteration_store.close_succeeded(
            self.iteration_id,
            outcomes=self._iteration_outcomes_for(attempt),
            closed_at=datetime.now(UTC),
        )
        self._on_iteration_closed(
            iteration_id=self.iteration_id,
            succeeded=True,
            deferred_goal=attempt.deferred_goal_for_next_iteration,
            final_attempt_id=attempt.id,
        )

    def _iteration_outcomes_for(self, attempt: Attempt) -> str:
        """JSON outcomes record for the passing attempt's reducers."""
        attempts = self._attempt_store.list_for_iteration(attempt.iteration_id)
        return records_json(project_iteration_outcomes(attempts, self._task_store))

    def _retry_or_close_failed(self, attempt: Attempt) -> None:
        while True:
            iteration = self._current_iteration_snapshot()
            if not iteration.has_budget_remaining:
                self._close_iteration_failed(attempt)
                return
            try:
                self.create_attempt(previous_attempt_id=attempt.id)
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
        # Failure-aware: denormalize the last failed attempt's failed-task
        # outcomes so the parent (or the run report) surfaces what went wrong.
        attempts = self._attempt_store.list_for_iteration(attempt.iteration_id)
        outcomes = records_json(project_iteration_outcomes(attempts, self._task_store))
        self._iteration_store.set_status(
            self.iteration_id,
            status=IterationStatus.FAILED,
            closed_at=datetime.now(UTC),
            outcomes=outcomes,
        )
        self._on_iteration_closed(
            iteration_id=self.iteration_id,
            succeeded=False,
            deferred_goal=None,
            final_attempt_id=attempt.id,
        )

    def _latest_failed_attempt_for(self, *, previous_id: str) -> Attempt | None:
        iteration = self._current_iteration_snapshot()
        latest_id = iteration.latest_attempt_id
        if latest_id is None or latest_id == previous_id:
            return None
        retry_attempt = self._attempt_store.get(latest_id)
        if retry_attempt is None or retry_attempt.status != AttemptStatus.FAILED:
            return None
        return retry_attempt


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
    "IterationClosedCallback",
    "IterationAttemptCoordinator",
    "OpenIterationCoordinatorRegistry",
    "OrchestratorFactory",
]
