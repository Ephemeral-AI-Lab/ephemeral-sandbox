"""Lifecycle controller for the top-level entry executor.

The entry executor is not itself a Mission. It is the top-level agent turn that
receives the user request and either completes directly or calls
``submit_execution_handoff`` to start the first delegated Mission.
"""

from __future__ import annotations

from dataclasses import asdict, dataclass
from typing import Any

from db.stores.task_center_store import TaskCenterStore
from task_center.exceptions import TaskCenterInvariantViolation
from task_center.mission.mission import MissionCloseReport
from task_center.task.models import TaskCenterTaskStatus


@dataclass(frozen=True, slots=True)
class EntryTaskController:
    """Single lifecycle owner for the entry executor task."""

    task_id: str
    task_center_run_id: str
    task_store: TaskCenterStore

    # ---- terminal events --------------------------------------------------

    def apply_executor_success(
        self, *, summary: str, artifacts: list[str]
    ) -> None:
        """Entry executor called ``submit_execution_success``."""
        if not self._mark_terminal(
            status=TaskCenterTaskStatus.DONE,
            summary={
                "outcome": "success",
                "summary": summary,
                "payload": {
                    "generator_role": "entry_executor",
                    "artifacts": artifacts,
                },
            },
        ):
            return
        self._finish_run(status="done")

    def apply_executor_failure(
        self, *, summary: str, reason: str, details: list[str]
    ) -> None:
        """Entry executor called ``submit_execution_failure``."""
        if not self._mark_terminal(
            status=TaskCenterTaskStatus.FAILED,
            summary={
                "outcome": "failure",
                "summary": summary,
                "payload": {
                    "generator_role": "entry_executor",
                    "reason": reason,
                    "details": details,
                },
            },
        ):
            return
        self._finish_run(status="failed")

    def apply_run_exhausted(self, *, summary: str) -> None:
        """Launcher detected the entry agent ended without a terminal."""
        if not self._mark_terminal(
            status=TaskCenterTaskStatus.FAILED,
            summary={
                "fail_reason": "run_exhausted",
                "summary": summary,
            },
        ):
            return
        self._finish_run(status="failed")

    # ---- delegated-mission resume -----------------------------------------

    def apply_mission_close_report(
        self, report: MissionCloseReport
    ) -> None:
        """Resume the entry task waiting on a delegated mission."""
        succeeded = report.outcome == "success"
        if succeeded:
            status = TaskCenterTaskStatus.DONE
            text = f"Delegated mission {report.mission_id} succeeded."
        else:
            status = TaskCenterTaskStatus.FAILED
            text = f"Delegated mission {report.mission_id} failed."

        try:
            updated = self.task_store.set_task_status_if_current(
                self.task_id,
                expected_status=TaskCenterTaskStatus.WAITING_MISSION.value,
                status=status.value,
                summary={
                    "outcome": report.outcome,
                    "summary": text,
                    "payload": {
                        "mission_close_report": asdict(report),
                        "submission_kind": "mission_close_report",
                    },
                },
            )
        except LookupError as exc:
            raise TaskCenterInvariantViolation(
                f"Entry task {self.task_id!r} not found"
            ) from exc
        if updated is None:
            return
        self._finish_run(status="done" if succeeded else "failed")

    # ---- waiting-on-delegated-mission -------------------------------------

    def mark_waiting_mission(
        self,
        *,
        delegated_mission_id: str,
        delegated_episode_id: str,
        delegated_attempt_id: str,
        goal: str,
    ) -> None:
        """Park the entry task in ``WAITING_MISSION``."""
        summary = {
            "outcome": "mission_start",
            "summary": "Waiting on delegated mission solution.",
            "payload": {
                "mission_id": delegated_mission_id,
                "initial_episode_id": delegated_episode_id,
                "initial_attempt_id": delegated_attempt_id,
                "parent_attempt_id": None,
                "goal": goal,
            },
        }
        updated = self.task_store.set_task_status_if_current(
            self.task_id,
            expected_status=TaskCenterTaskStatus.RUNNING.value,
            status=TaskCenterTaskStatus.WAITING_MISSION.value,
            summary=summary,
        )
        if updated is None:
            raise TaskCenterInvariantViolation(
                f"Entry task {self.task_id!r} was not running when the "
                "delegated mission start tried to mark it waiting."
            )

    def restore_running_after_failed_mission_start(self) -> None:
        """Roll the entry task back to RUNNING after a failed mission start."""
        self.task_store.set_task_status_if_current(
            self.task_id,
            expected_status=TaskCenterTaskStatus.WAITING_MISSION.value,
            status=TaskCenterTaskStatus.RUNNING.value,
        )

    # ---- internal ----------------------------------------------------------

    def _mark_terminal(
        self,
        *,
        status: TaskCenterTaskStatus,
        summary: dict[str, Any],
    ) -> bool:
        """CAS the entry task from RUNNING to *status*."""
        try:
            updated = self.task_store.set_task_status_if_current(
                self.task_id,
                expected_status=TaskCenterTaskStatus.RUNNING.value,
                status=status.value,
                summary=summary,
            )
        except LookupError as exc:
            raise TaskCenterInvariantViolation(
                f"Entry task {self.task_id!r} not found"
            ) from exc
        return updated is not None

    def _finish_run(self, *, status: str) -> None:
        run = self.task_store.get_run(self.task_center_run_id)
        if run is None or run.get("status") in ("done", "failed"):
            return
        self.task_store.finish_run(self.task_center_run_id, status=status)


__all__ = ["EntryTaskController"]
