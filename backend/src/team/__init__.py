"""Agent team orchestration layer.

A minimal wrapper on top of ``engine.core.query.run_query`` that adds a DAG
of ``WorkItem`` nodes, dependency-aware scheduling, and planner agents that
extend the DAG via ``submit_plan``. Non-team mode (direct ``run_query``) is
untouched — deleting ``backend/src/team/`` leaves the single-agent flow
fully functional.
"""

from team.types import (
    AgentResult,
    BudgetConfig,
    BudgetState,
    Plan,
    TeamDefinition,
    TeamRunStatus,
    WorkItem,
    WorkItemSpec,
    WorkItemStatus,
)

__all__ = [
    "AgentResult",
    "BudgetConfig",
    "BudgetState",
    "Plan",
    "TeamDefinition",
    "TeamRunStatus",
    "WorkItem",
    "WorkItemSpec",
    "WorkItemStatus",
]
