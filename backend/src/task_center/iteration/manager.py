"""Episode lifecycle manager and process-local registry.

``EpisodeManager`` is the sole creator of Attempt records inside its owned
episode and the only emitter of ``EpisodeClosureReport``.
``EpisodeManagerRegistry`` is the process-local one-manager-per-open-episode
registry.
"""

from __future__ import annotations

import logging
from collections.abc import Callable
from datetime import UTC, datetime

from task_center._core.infra import (
    assert_attempt_belongs_to_episode,
    assert_attempt_sequence_contiguous,
    assert_episode_has_budget,
    assert_episode_open,
    assert_fail_reason_present_on_failure,
)
from task_center._core.persistence import (
    AttemptStoreProtocol,
    EpisodeStoreProtocol,
    TaskStoreProtocol,
)
from task_center._core.types import (
    RegisteredAttemptOrchestrator,
    TaskCenterInvariantViolation,
)
from task_center.attempt.state import (
    Attempt,
    AttemptFailReason,
    AttemptStatus,
)
from task_center.episode.state import (
    AttemptedPlanEntry,
    AttemptPlanFailed,
    Episode,
    EpisodeClosureReport,
    EpisodeStatus,
    SuccessContinue,
    TerminalSuccess,
)

logger = logging.getLogger(__name__)


ClosureReportSink = Callable[[EpisodeClosureReport], None]
AttemptClosedCallback = Callable[[str], None]
OrchestratorFactory = Callable[
    [Attempt, AttemptClosedCallback], RegisteredAttemptOrchestrator
]


class EpisodeManager:
    """Manages one open Episode's lifecycle."""

    def __init__(
        self,
        *,
        episode_id: str,
        episode_store: EpisodeStoreProtocol,
        attempt_store: AttemptStoreProtocol,
        on_episode_closed: ClosureReportSink,
        orchestrator_factory: OrchestratorFactory | None = None,
        task_store: TaskStoreProtocol | None = None,
    ) -> None:
        self.episode_id = episode_id
        self._episode_store = episode_store
        self._attempt_store = attempt_store
        self._on_episode_closed = on_episode_closed
        self._orchestrator_factory = orchestrator_factory
        # Optional — when present, the manager denormalizes the evaluator's
        # pass-summary text onto the episode row at successful close so the
        # context engine's planner recipe can read it on retry / chain.
        self._task_store = task_store

    # ---- public API -----------------------------------------------------

    def create_initial_attempt(self) -> Attempt:
        """Create attempt sequence 1 and start its orchestrator."""
        attempt = self.create_unstarted_initial_attempt()
        self.start_attempt(attempt)
        return attempt

    def create_unstarted_initial_attempt(self) -> Attempt:
        """Create attempt sequence 1 without starting its orchestrator."""
        episode = self._current_episode_snapshot()
        assert_episode_open(episode)
        if episode.attempt_ids:
            raise TaskCenterInvariantViolation(
                f"Episode {episode.id!r} already has attempts; use "
                f"create_next_attempt"
            )
        return self._insert_attempt(episode, attempt_sequence_no=1)

    def start_attempt(self, attempt: Attempt) -> None:
        """Start an attempt that belongs to this manager's open episode."""
        episode = self._current_episode_snapshot()
        assert_episode_open(episode)
        assert_attempt_belongs_to_episode(attempt, episode)
        self._start_orchestrator_if_configured(attempt)

    def create_next_attempt(
        self, *, previous_attempt_id: str
    ) -> Attempt:
        """Called after a failed attempt if the episode still has budget."""
        episode = self._current_episode_snapshot()
        assert_episode_open(episode)
        assert_episode_has_budget(episode)
        if episode.latest_attempt_id != previous_attempt_id:
            raise TaskCenterInvariantViolation(
                f"previous_attempt_id {previous_attempt_id!r} is not "
                f"the latest attempt of episode {episode.id!r} "
                f"(latest={episode.latest_attempt_id!r})"
            )
        attempt = self._insert_attempt(
            episode, attempt_sequence_no=episode.attempt_count + 1
        )
        self._start_orchestrator_if_configured(attempt)
        return attempt

    def handle_attempt_closed(self, attempt_id: str) -> None:
        """Entry point for the closed-attempt callback from the orchestrator."""
        attempt = self._attempt_store.get(attempt_id)
        if attempt is None:
            raise TaskCenterInvariantViolation(
                f"Attempt {attempt_id!r} not found"
            )
        episode = self._current_episode_snapshot()
        assert_episode_open(episode)
        assert_attempt_belongs_to_episode(attempt, episode)
        assert_fail_reason_present_on_failure(attempt)

        if attempt.status == AttemptStatus.PASSED:
            self._close_episode_passed(attempt)
        else:
            self._retry_or_close_failed(attempt)

    # ---- internal -------------------------------------------------------

    def _current_episode_snapshot(self) -> Episode:
        episode = self._episode_store.get(self.episode_id)
        if episode is None:
            raise TaskCenterInvariantViolation(
                f"Episode {self.episode_id!r} not found"
            )
        return episode

    def _insert_attempt(
        self, episode: Episode, *, attempt_sequence_no: int
    ) -> Attempt:
        assert_attempt_sequence_contiguous(episode, attempt_sequence_no)
        attempt = self._attempt_store.insert(
            episode_id=episode.id,
            attempt_sequence_no=attempt_sequence_no,
        )
        self._episode_store.append_attempt_id(episode.id, attempt.id)
        return attempt

    def _start_orchestrator_if_configured(self, attempt: Attempt) -> None:
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
                "EpisodeManager: startup attempt cleanup failed",
            )

    def _close_episode_passed(self, attempt: Attempt) -> None:
        self._episode_store.set_continuation_goal(
            self.episode_id, attempt.continuation_goal
        )
        # Atomically transition status + write the denormalized
        # task_specification (from the passing attempt) and task_summary
        # (from the evaluator's pass summary text) onto the episode row.
        self._episode_store.close_succeeded(
            self.episode_id,
            task_specification=attempt.task_specification or "",
            task_summary=self._evaluator_pass_summary_for(attempt),
            closed_at=datetime.now(UTC),
        )
        if attempt.continuation_goal is None:
            self._emit_terminal_success(attempt)
        else:
            self._emit_success_continue(attempt)

    def _evaluator_pass_summary_for(self, attempt: Attempt) -> str:
        """Resolve the evaluator's success-summary text for *attempt*.

        Empty string when the manager is configured without a ``task_store``
        (test seams) or when the evaluator never recorded a summary.
        """
        if self._task_store is None:
            return ""
        return self._task_store.get_evaluator_pass_summary(attempt.id)

    def _retry_or_close_failed(self, attempt: Attempt) -> None:
        while True:
            episode = self._current_episode_snapshot()
            if not episode.has_budget_remaining:
                self._close_episode_failed(attempt)
                return
            try:
                self.create_next_attempt(previous_attempt_id=attempt.id)
                return
            except Exception:
                # Retry start failed; the new attempt was inserted and closed
                # STARTUP_FAILED before the exception propagated. Re-enter the
                # retry decision on the new closed attempt instead of leaving
                # the episode open.
                retry_attempt = self._latest_failed_attempt_for(
                    previous_id=attempt.id
                )
                if retry_attempt is None:
                    raise
                logger.warning(
                    "EpisodeManager: retry start failure for episode %r; "
                    "treating new attempt %r as a failed attempt",
                    self.episode_id,
                    retry_attempt.id,
                    exc_info=True,
                )
                attempt = retry_attempt
                continue

    def _close_episode_failed(self, attempt: Attempt) -> None:
        self._episode_store.set_status(
            self.episode_id,
            status=EpisodeStatus.FAILED,
            closed_at=datetime.now(UTC),
        )
        self._emit_attempt_plan_failed(attempt)

    def _latest_failed_attempt_for(
        self, *, previous_id: str
    ) -> Attempt | None:
        episode = self._current_episode_snapshot()
        latest_id = episode.latest_attempt_id
        if latest_id is None or latest_id == previous_id:
            return None
        retry_attempt = self._attempt_store.get(latest_id)
        if retry_attempt is None or retry_attempt.status != AttemptStatus.FAILED:
            return None
        return retry_attempt

    def _emit_terminal_success(self, attempt: Attempt) -> None:
        report = EpisodeClosureReport(
            episode_id=self.episode_id,
            final_attempt_id=attempt.id,
            outcome=TerminalSuccess(),
        )
        self._on_episode_closed(report)

    def _emit_success_continue(self, attempt: Attempt) -> None:
        if attempt.continuation_goal is None:
            raise TaskCenterInvariantViolation(
                "success_continue requires a non-null continuation_goal"
            )
        report = EpisodeClosureReport(
            episode_id=self.episode_id,
            final_attempt_id=attempt.id,
            outcome=SuccessContinue(goal=attempt.continuation_goal),
        )
        self._on_episode_closed(report)

    def _emit_attempt_plan_failed(self, last_attempt: Attempt) -> None:
        history = self._build_attempted_plan_history()
        report = EpisodeClosureReport(
            episode_id=self.episode_id,
            final_attempt_id=last_attempt.id,
            outcome=AttemptPlanFailed(
                failure_summary=(
                    last_attempt.fail_reason.value
                    if last_attempt.fail_reason is not None
                    else "unknown"
                ),
                attempted_plan_history=history,
            ),
        )
        self._on_episode_closed(report)

    def _build_attempted_plan_history(self) -> tuple[AttemptedPlanEntry, ...]:
        attempts = self._attempt_store.list_for_episode(self.episode_id)
        return tuple(
            AttemptedPlanEntry(
                attempt_id=g.id,
                attempt_sequence_no=g.attempt_sequence_no,
                task_specification=g.task_specification,
                evaluation_criteria=g.evaluation_criteria,
                fail_reason=g.fail_reason,
                attempt_summary_id=None,
                failure_landscape=None,
            )
            for g in attempts
        )


class EpisodeManagerRegistry:
    """In-memory registry enforcing one-manager-per-open-episode."""

    def __init__(self) -> None:
        self._by_episode_id: dict[str, EpisodeManager] = {}

    def register(self, manager: EpisodeManager) -> None:
        episode_id = manager.episode_id
        if episode_id in self._by_episode_id:
            raise TaskCenterInvariantViolation(
                f"EpisodeManager already registered for episode {episode_id!r}"
            )
        self._by_episode_id[episode_id] = manager

    def get(self, episode_id: str) -> EpisodeManager | None:
        return self._by_episode_id.get(episode_id)

    def deregister(self, episode_id: str) -> None:
        self._by_episode_id.pop(episode_id, None)


__all__ = [
    "AttemptClosedCallback",
    "ClosureReportSink",
    "EpisodeManager",
    "EpisodeManagerRegistry",
    "OrchestratorFactory",
]
