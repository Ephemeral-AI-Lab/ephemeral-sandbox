"""Team-mode runtime exports.

Keep package imports light so unit tests can import narrow runtime helpers
without pulling the full persistence stack at module import time.
"""

from __future__ import annotations

from importlib import import_module
from typing import Any

__all__ = ["Executor", "TaskQueue", "TaskStatusHandler", "TeamRun"]


def __getattr__(name: str) -> Any:
    if name == "Executor":
        return import_module("team.runtime.executor").Executor
    if name == "TaskQueue":
        return import_module("team.runtime.task_queue").TaskQueue
    if name == "TaskStatusHandler":
        return import_module("team.runtime.status_handler").TaskStatusHandler
    if name == "TeamRun":
        return import_module("team.runtime.team_run").TeamRun
    raise AttributeError(name)
