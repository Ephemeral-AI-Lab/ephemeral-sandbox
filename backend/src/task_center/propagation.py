"""Summary propagation along the closes_for chain.

When a leaf in the closure chain calls ``submit_task_completion(summary)``,
walk ``closes_for`` from the leaf upward, copying ``summary`` and setting
``status=DONE`` on every node in the chain.
"""

from __future__ import annotations

from typing import Mapping

from task_center.errors import TaskCenterError
from task_center.task import Status, Task, TaskId


def close_with_summary(
    tasks: Mapping[TaskId, Task],
    task_id: TaskId,
    summary: str,
) -> list[TaskId]:
    """Walk ``closes_for`` from ``task_id``, set summary + status=DONE on each.

    Args:
        tasks: Mapping of id to :class:`Task` (typically ``TaskGraph.tasks``).
        task_id: The leaf task that called ``submit_task_completion``.
        summary: The closure summary to propagate.

    Returns:
        The list of task ids that were closed, in walk order (leaf first,
        then ancestors via ``closes_for``).

    Raises:
        TaskCenterError: if any id along the chain is not present in ``tasks``.
    """
    closed: list[TaskId] = []
    cur_id: TaskId | None = task_id
    while cur_id is not None:
        task = tasks.get(cur_id)
        if task is None:
            raise TaskCenterError(
                f"close_with_summary: task id {cur_id!r} not in tasks map"
            )
        task.summary = summary
        task.status = Status.DONE
        closed.append(cur_id)
        cur_id = task.closes_for
    return closed
