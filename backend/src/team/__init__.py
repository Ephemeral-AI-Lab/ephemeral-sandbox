"""Agent team orchestration layer.

A minimal wrapper on top of ``engine.core.query.run_query`` that adds a DAG
of ``Task`` nodes, dependency-aware scheduling, and planner agents that
extend the DAG via ``submit_plan``. Non-team mode (direct ``run_query``) is
untouched — deleting ``backend/src/team/`` leaves the single-agent flow
fully functional.
"""

from __future__ import annotations

from importlib import import_module
from typing import Any

from team.errors import (
    BudgetExceeded,
    GraphInvariantViolation,
    InvalidPlan,
)
from team.models import (
    AgentResult,
    BudgetConfig,
    BudgetState,
    Plan,
    Task,
    TaskDefinition,
    TaskStatus,
    TERMINAL_STATUSES,
    TeamDefinition,
    TeamRunStatus,
)

__all__ = [
    "AgentResult",
    "BudgetConfig",
    "BudgetExceeded",
    "BudgetState",
    "GraphInvariantViolation",
    "InvalidPlan",
    "Plan",
    "Task",
    "TaskDefinition",
    "TaskStatus",
    "TERMINAL_STATUSES",
    "TeamDefinition",
    "TeamRun",
    "TeamRunStatus",
]


def __getattr__(name: str) -> Any:
    if name == "TeamRun":
        return import_module("team.runtime.team_run").TeamRun
    raise AttributeError(name)
