"""Generator DAG helper functions for one harness attempt."""

from __future__ import annotations

from collections import deque
from typing import Any

from task_center._core.types import TaskCenterInvariantViolation
from task_center._core.types import generator_task_id
from task_center.task_state import (
    TaskCenterTaskStatus,
    PlannedGeneratorTask,
    TERMINAL_GENERATOR_STATUSES,
)


def ordered_generator_tasks(
    tasks: tuple[PlannedGeneratorTask, ...]
) -> tuple[PlannedGeneratorTask, ...]:
    local_ids = {task.local_id for task in tasks}
    if len(local_ids) != len(tasks):
        raise TaskCenterInvariantViolation("Generator plan contains duplicate local ids")
    for task in tasks:
        missing = [dep for dep in task.deps if dep not in local_ids]
        if missing:
            raise TaskCenterInvariantViolation(
                f"Generator task {task.local_id!r} has unknown deps: {missing!r}"
            )

    by_id = {task.local_id: task for task in tasks}
    remaining_deps = {task.local_id: set(task.deps) for task in tasks}
    dependents: dict[str, list[str]] = {task.local_id: [] for task in tasks}
    for task in tasks:
        for dep in task.deps:
            dependents[dep].append(task.local_id)

    ready = deque(task.local_id for task in tasks if not task.deps)
    ordered: list[PlannedGeneratorTask] = []
    while ready:
        local_id = ready.popleft()
        ordered.append(by_id[local_id])
        for dependent_id in dependents[local_id]:
            remaining_deps[dependent_id].discard(local_id)
            if not remaining_deps[dependent_id]:
                ready.append(dependent_id)

    if len(ordered) != len(tasks):
        raise TaskCenterInvariantViolation("Generator plan contains a dependency cycle")
    return tuple(ordered)


def dependency_task_ids(
    *,
    attempt_id: str,
    local_deps: tuple[str, ...],
) -> tuple[str, ...]:
    return tuple(generator_task_id(attempt_id, dep) for dep in local_deps)


TaskRecord = dict[str, Any]


def generator_status_map(
    task_records: list[TaskRecord],
) -> dict[str, TaskCenterTaskStatus]:
    return {
        task["id"]: TaskCenterTaskStatus(task["status"])
        for task in task_records
    }


def ready_pending_generator_ids(task_records: list[TaskRecord]) -> tuple[str, ...]:
    statuses = generator_status_map(task_records)
    ready: list[str] = []
    for task in task_records:
        if statuses[task["id"]] != TaskCenterTaskStatus.PENDING:
            continue
        needs = tuple(task.get("needs") or ())
        missing = [dep for dep in needs if dep not in statuses]
        if missing:
            raise TaskCenterInvariantViolation(
                f"Generator task {task['id']!r} has unknown persisted deps: "
                f"{missing!r}"
            )
        if all(statuses[dep] == TaskCenterTaskStatus.DONE for dep in needs):
            ready.append(task["id"])
    return tuple(ready)


def blocked_descendant_ids(
    *,
    failed_task_id: str,
    task_records: list[TaskRecord],
) -> tuple[str, ...]:
    statuses = generator_status_map(task_records)
    if failed_task_id not in statuses:
        raise TaskCenterInvariantViolation(
            f"Failed generator task {failed_task_id!r} is not in this attempt"
        )

    dependents: dict[str, list[str]] = {task["id"]: [] for task in task_records}
    for task in task_records:
        for dep in task.get("needs") or ():
            if dep not in statuses:
                raise TaskCenterInvariantViolation(
                    f"Generator task {task['id']!r} has unknown persisted dep "
                    f"{dep!r}"
                )
            dependents[dep].append(task["id"])

    blocked: list[str] = []
    queue = deque(dependents[failed_task_id])
    seen: set[str] = set()
    while queue:
        task_id = queue.popleft()
        if task_id in seen:
            continue
        seen.add(task_id)
        status = statuses[task_id]
        if status not in (
            TaskCenterTaskStatus.PENDING,
            TaskCenterTaskStatus.BLOCKED,
        ):
            raise TaskCenterInvariantViolation(
                f"Non-pending generator task {task_id!r} depends on failed task "
                f"{failed_task_id!r}"
            )
        if status == TaskCenterTaskStatus.PENDING:
            blocked.append(task_id)
        for child_id in dependents[task_id]:
            queue.append(child_id)
    return tuple(blocked)


def all_generators_quiescent(task_records: list[TaskRecord]) -> bool:
    statuses = generator_status_map(task_records).values()
    return all(status in TERMINAL_GENERATOR_STATUSES for status in statuses)


def all_generators_done(task_records: list[TaskRecord]) -> bool:
    statuses = generator_status_map(task_records).values()
    return all(status == TaskCenterTaskStatus.DONE for status in statuses)


def any_generator_failed_or_blocked(task_records: list[TaskRecord]) -> bool:
    statuses = generator_status_map(task_records).values()
    return any(
        status in (TaskCenterTaskStatus.FAILED, TaskCenterTaskStatus.BLOCKED)
        for status in statuses
    )
