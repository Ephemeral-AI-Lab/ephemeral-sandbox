"""Exception types raised by the task_center module."""

from __future__ import annotations


class TaskCenterError(Exception):
    """Base class for all task_center errors."""


class PlanValidationError(TaskCenterError):
    """Raised by ``compile_dag`` when an executor's submitted plan fails validation."""
