"""Agent team orchestration layer.

A minimal wrapper on top of ``engine.core.query.run_query`` that adds a DAG
of ``WorkItem`` nodes, dependency-aware scheduling, and planner agents that
extend the DAG via ``submit_plan``. Non-team mode (direct ``run_query``) is
untouched — deleting ``backend/src/team/`` leaves the single-agent flow
fully functional.
"""

from team.errors import (
    ArtifactTooLarge,
    BudgetExceeded,
    CheckpointNotFound,
    InvalidPlan,
    NoPosthookOutput,
)
from team.models import (
    AgentResult,
    BudgetConfig,
    BudgetState,
    Plan,
    TeamDefinition,
    TeamRunStatus,
    WorkItem,
    WorkItemKind,
    WorkItemSpec,
    WorkItemStatus,
)
from team.runtime.team_run import TeamRun

__all__ = [
    "AgentResult",
    "ArtifactTooLarge",
    "BudgetConfig",
    "BudgetExceeded",
    "BudgetState",
    "CheckpointNotFound",
    "InvalidPlan",
    "NoPosthookOutput",
    "Plan",
    "TeamDefinition",
    "TeamRun",
    "TeamRunStatus",
    "WorkItem",
    "WorkItemKind",
    "WorkItemSpec",
    "WorkItemStatus",
]
