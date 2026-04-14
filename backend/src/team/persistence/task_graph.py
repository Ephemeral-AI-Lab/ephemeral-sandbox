"""TaskGraph — in-memory cache of tasks + ready-queue ordering.

Extracted from TaskStore so SQL persistence and in-memory state
are separate concerns. TaskStore owns a TaskGraph instance and
delegates every in-memory mutation to it.
"""

from __future__ import annotations

from collections.abc import Iterable
from typing import TYPE_CHECKING

from team.models import Task, TaskStatus

if TYPE_CHECKING:
    pass


class TaskGraph:
    """In-memory mirror of persisted tasks and the ready-queue ordering.

    Exposes ``tasks`` (dict by id) and ``ready_order`` (ids in READY status,
    insertion order) as public attributes so external readers can iterate.
    All writes should go through the methods below.
    """

    def __init__(self) -> None:
        self.tasks: dict[str, Task] = {}
        self.ready_order: list[str] = []

    # ---- load / replace ---------------------------------------------------

    def load(self, tasks: Iterable[Task]) -> dict[str, Task]:
        """Replace all in-memory state from a fresh task list."""
        tasks = list(tasks)
        self.tasks = {t.id: t for t in tasks}
        self.ready_order = [t.id for t in tasks if t.status == TaskStatus.READY]
        return self.tasks

    # ---- ready-queue primitives -------------------------------------------

    def add_ready(self, task_id: str) -> None:
        if task_id not in self.ready_order:
            self.ready_order.append(task_id)

    def remove_ready(self, task_id: str) -> None:
        if task_id in self.ready_order:
            self.ready_order.remove(task_id)

    # ---- status transitions -----------------------------------------------

    def mark_done(self, task_id: str, promoted_ids: Iterable[str]) -> None:
        task = self.tasks.get(task_id)
        if task is not None:
            task.status = TaskStatus.DONE
        self.remove_ready(task_id)
        for pid in promoted_ids:
            promoted = self.tasks.get(pid)
            if promoted is not None:
                promoted.status = TaskStatus.READY
            self.add_ready(pid)

    def mark_expanded(self, task_id: str) -> None:
        task = self.tasks.get(task_id)
        if task is not None:
            task.status = TaskStatus.EXPANDED

    def mark_terminal(self, task_id: str, status: str, reason: str) -> None:
        task = self.tasks.get(task_id)
        if task is not None:
            task.status = TaskStatus(status)
            task.failure_reason = reason
        self.remove_ready(task_id)

    def mark_cancelled(self, ids: Iterable[str]) -> None:
        for cid in ids:
            task = self.tasks.get(cid)
            if task is not None:
                task.status = TaskStatus.CANCELLED
            self.remove_ready(cid)

    def mark_failed(self, task_id: str, reason: str | None = None) -> None:
        """Mark task FAILED and drop from ready_order.

        ``reason`` may be omitted when the caller only wants to update status
        (e.g. retry_task's terminal failure after DB commit).
        """
        task = self.tasks.get(task_id)
        if task is not None:
            task.status = TaskStatus.FAILED
            if reason is not None:
                task.failure_reason = reason
        self.remove_ready(task_id)

    def set_ready_status(self, task_id: str) -> None:
        """Update status to READY without touching ready_order."""
        task = self.tasks.get(task_id)
        if task is not None:
            task.status = TaskStatus.READY

    def requeue_ready(self, task_id: str) -> None:
        """Set task status to READY and ensure it is in ready_order."""
        task = self.tasks.get(task_id)
        if task is not None:
            task.status = TaskStatus.READY
        self.add_ready(task_id)

    def pause(
        self,
        task_id: str,
        blocker_id: str,
        checkpoint: str,
        verdict: str,
    ) -> None:
        task = self.tasks.get(task_id)
        if task is None:
            return
        task.status = TaskStatus.PAUSED
        task.blocker_id = blocker_id
        task.pause_checkpoint = checkpoint
        task.pause_verdict = verdict

    # ---- insertion --------------------------------------------------------

    def insert_tasks(self, tasks: Iterable[Task]) -> None:
        """Upsert new tasks; any with READY status join the ready queue."""
        for task in tasks:
            self.tasks[task.id] = task
            if task.status == TaskStatus.READY:
                self.add_ready(task.id)

    def upsert(self, task: Task, *, enqueue_if_ready: bool = False) -> None:
        """Insert or overwrite a single task.

        By default does not touch ``ready_order`` (used for RUNNING tasks
        picked up by the executor). Pass ``enqueue_if_ready=True`` to also
        add to the ready queue when the task is READY.
        """
        self.tasks[task.id] = task
        if enqueue_if_ready and task.status == TaskStatus.READY:
            self.add_ready(task.id)

    def recover_running(self, tasks: Iterable[Task]) -> None:
        """Restore crashed-running tasks to ready state in memory."""
        for task in tasks:
            self.tasks[task.id] = task
            self.add_ready(task.id)

    # ---- replan -----------------------------------------------------------

    def apply_replan(
        self,
        *,
        failed_task_id: str,
        reason: str,
        replanner_task: Task,
    ) -> None:
        """Mark original task FAILED and insert the replanner task."""
        original = self.tasks.get(failed_task_id)
        if original is not None:
            original.status = TaskStatus.REPLANNING
            original.failure_reason = f"replan_requested: {reason}"
        self.remove_ready(failed_task_id)
        self.tasks[replanner_task.id] = replanner_task
        if replanner_task.status == TaskStatus.READY:
            self.add_ready(replanner_task.id)
