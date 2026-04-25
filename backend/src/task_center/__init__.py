"""Per-session task graph orchestrator for the executor-evaluator tree.

Public surface:

- :class:`Task`, :class:`Status`, :data:`TaskRole`, :data:`TaskId` —
  the data model.
- :class:`TaskCenterError`, :class:`PlanValidationError` — error hierarchy.
- :func:`compile_dag` — DAG plan validator + dep compiler.
"""

from __future__ import annotations

from task_center.dag import compile_dag
from task_center.errors import PlanValidationError, TaskCenterError
from task_center.task import (
    Status,
    Task,
    TaskId,
    TaskRole,
)

__all__ = [
    "PlanValidationError",
    "Status",
    "Task",
    "TaskCenterError",
    "TaskId",
    "TaskRole",
    "compile_dag",
]
