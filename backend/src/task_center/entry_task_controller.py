"""EntryTaskController — lifecycle controller for the graph-less entry executor.

Receives the lifecycle events that a :class:`HarnessGraphOrchestrator` would
handle in graph mode (terminal submissions, run exhaustion, delegated
complex-task close reports). The entry executor lives in a
:class:`TaskSegment` with **zero** ``HarnessGraph`` rows (per phase-06
*Sources of truth*: an entry segment may have zero ``HarnessGraph`` rows);
this controller is the single owner of:

    - entry-task status transitions (RUNNING ↔ WAITING_COMPLEX_TASK ↔ DONE/FAILED)
    - entry-segment close (no graph rows to drive the manager retry path)
    - entry-request close via :class:`ComplexTaskRequestHandler`
    - run finalization via the handler's ``deliver_close_report`` callback

Construction is owned by :class:`TaskCenterEntryCoordinator`; the controller
is attached to :class:`HarnessGraphRuntime.entry_task_controller` so the
close-report router and launcher can dispatch into it without further
plumbing.
"""

from __future__ import annotations

from dataclasses import asdict, dataclass
from datetime import UTC, datetime
from typing import Any

from db.stores.task_center_store import TaskCenterStore
from db.stores.task_segment_store import TaskSegmentStore
from task_center.mission.handler import ComplexTaskRequestHandler
from task_center.mission.mission import ComplexTaskCloseReport
from task_center.exceptions import GraphInvariantViolation
from task_center.episode.registry import SegmentManagerRegistry
from task_center.episode.episode import TaskSegmentStatus
from task_center.task import HarnessTaskStatus


@dataclass(frozen=True, slots=True)
class EntryTaskController:
    """Single lifecycle owner for the graph-less entry executor task."""

    task_id: str
    task_center_run_id: str
    complex_task_request_id: str
    task_segment_id: str
    task_store: TaskCenterStore
    segment_store: TaskSegmentStore
    request_handler: ComplexTaskRequestHandler
    manager_registry: SegmentManagerRegistry

    # ---- terminal events --------------------------------------------------

    def apply_executor_success(
        self, *, summary: str, artifacts: list[str]
    ) -> None:
        """Entry executor called ``submit_execution_success``.

        Marks the entry task DONE, closes the entry segment as succeeded,
        and closes the entry request — which in turn finalizes the run via
        the handler's ``deliver_close_report`` callback.
        """
        if not self._mark_terminal(
            status=HarnessTaskStatus.DONE,
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
        self._close_segment_and_request(
            succeeded=True,
            task_specification=summary,
            task_summary=summary,
        )

    def apply_executor_failure(
        self, *, summary: str, reason: str, details: list[str]
    ) -> None:
        """Entry executor called ``submit_execution_failure``."""
        if not self._mark_terminal(
            status=HarnessTaskStatus.FAILED,
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
        self._close_segment_and_request(
            succeeded=False,
            task_specification=summary,
            task_summary=summary,
        )

    def apply_run_exhausted(self, *, summary: str) -> None:
        """Launcher detected the entry agent ended without a terminal."""
        if not self._mark_terminal(
            status=HarnessTaskStatus.FAILED,
            summary={
                "fail_reason": "run_exhausted",
                "summary": summary,
            },
        ):
            return
        self._close_segment_and_request(
            succeeded=False,
            task_specification=summary,
            task_summary=summary,
        )

    # ---- delegated-complex-task resume ------------------------------------

    def apply_complex_task_close_report(
        self, report: ComplexTaskCloseReport
    ) -> None:
        """Resume the entry task waiting on a delegated complex-task request.

        Idempotent: the CAS with ``expected_status=WAITING_COMPLEX_TASK``
        returns ``None`` when the entry task has already moved off (earlier
        delivery, terminal already fired) — no pre-read needed.
        """
        succeeded = report.outcome == "success"
        if succeeded:
            status = HarnessTaskStatus.DONE
            text = (
                f"Delegated complex task {report.complex_task_request_id} "
                "succeeded."
            )
        else:
            status = HarnessTaskStatus.FAILED
            text = (
                f"Delegated complex task {report.complex_task_request_id} "
                "failed."
            )

        try:
            updated = self.task_store.set_task_status_if_current(
                self.task_id,
                expected_status=HarnessTaskStatus.WAITING_COMPLEX_TASK.value,
                status=status.value,
                summary={
                    "outcome": report.outcome,
                    "summary": text,
                    "payload": {
                        "complex_task_close_report": asdict(report),
                        "submission_kind": "complex_task_close_report",
                    },
                },
            )
        except LookupError as exc:
            raise GraphInvariantViolation(
                f"Entry task {self.task_id!r} not found"
            ) from exc
        if updated is None:
            return  # CAS miss: already delivered or already terminal.
        self._close_segment_and_request(
            succeeded=succeeded,
            task_specification=text,
            task_summary=text,
        )

    # ---- waiting-on-delegated-request -------------------------------------

    def mark_waiting_complex_task(
        self,
        *,
        delegated_request_id: str,
        delegated_segment_id: str,
        delegated_graph_id: str,
        goal: str,
    ) -> None:
        """Park the entry task in ``WAITING_COMPLEX_TASK``.

        Called by the mission starter when the entry executor invokes
        ``request_complex_task_solution``.
        """
        summary = {
            "outcome": "complex_task_request_start",
            "summary": "Waiting on delegated complex task solution.",
            "payload": {
                "complex_task_request_id": delegated_request_id,
                "initial_segment_id": delegated_segment_id,
                "initial_harness_graph_id": delegated_graph_id,
                "parent_harness_graph_id": None,
                "goal": goal,
            },
        }
        updated = self.task_store.set_task_status_if_current(
            self.task_id,
            expected_status=HarnessTaskStatus.RUNNING.value,
            status=HarnessTaskStatus.WAITING_COMPLEX_TASK.value,
            summary=summary,
        )
        if updated is None:
            raise GraphInvariantViolation(
                f"Entry task {self.task_id!r} was not running when the "
                "delegated mission start tried to mark it waiting."
            )

    def restore_running_after_failed_mission_start(self) -> None:
        """Roll the entry task back to RUNNING after a failed mission start.

        Mirror image of :meth:`mark_waiting_complex_task` for compensation.
        """
        self.task_store.set_task_status_if_current(
            self.task_id,
            expected_status=HarnessTaskStatus.WAITING_COMPLEX_TASK.value,
            status=HarnessTaskStatus.RUNNING.value,
        )

    # ---- internal ----------------------------------------------------------

    def _mark_terminal(
        self,
        *,
        status: HarnessTaskStatus,
        summary: dict[str, Any],
    ) -> bool:
        """CAS the entry task from RUNNING to *status*.

        Returns ``True`` when the transition happened, ``False`` when the
        task was already off RUNNING (terminal raced ahead, or the entry
        was parked in WAITING_COMPLEX_TASK and resumed via close-report).
        Idempotent at the CAS level — no pre-read needed.
        """
        try:
            updated = self.task_store.set_task_status_if_current(
                self.task_id,
                expected_status=HarnessTaskStatus.RUNNING.value,
                status=status.value,
                summary=summary,
            )
        except LookupError as exc:
            raise GraphInvariantViolation(
                f"Entry task {self.task_id!r} not found"
            ) from exc
        return updated is not None

    def _close_segment_and_request(
        self,
        *,
        succeeded: bool,
        task_specification: str,
        task_summary: str,
    ) -> None:
        """Close the entry segment + entry complex_request.

        Closing the request triggers ``deliver_close_report`` (wired by the
        entry coordinator) which finishes the run.
        """
        self._close_entry_segment(
            succeeded=succeeded,
            task_specification=task_specification,
            task_summary=task_summary,
        )
        self.manager_registry.deregister(self.task_segment_id)
        self.request_handler.close_mission_request(
            complex_task_request_id=self.complex_task_request_id,
            succeeded=succeeded,
            final_segment_id=self.task_segment_id,
            final_harness_graph_id=None,
        )

    def _close_entry_segment(
        self,
        *,
        succeeded: bool,
        task_specification: str,
        task_summary: str,
    ) -> None:
        """Atomically close the entry segment.

        Idempotent: if the segment is already closed, no-op.
        """
        segment = self.segment_store.get(self.task_segment_id)
        if segment is None:
            raise GraphInvariantViolation(
                f"Entry segment {self.task_segment_id!r} not found"
            )
        if segment.status != TaskSegmentStatus.OPEN:
            return
        now = datetime.now(UTC)
        if succeeded:
            self.segment_store.close_succeeded(
                self.task_segment_id,
                task_specification=task_specification,
                task_summary=task_summary,
                closed_at=now,
            )
        else:
            self.segment_store.set_status(
                self.task_segment_id,
                status=TaskSegmentStatus.FAILED,
                closed_at=now,
            )
