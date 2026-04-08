"""Checkpoint snapshot dataclass for a TeamRun."""

from __future__ import annotations

from dataclasses import dataclass
from datetime import datetime
from typing import Any

from team.models import BudgetState, WorkItem


@dataclass
class TeamRunCheckpoint:
    id: str
    team_run_id: str
    sequence: int
    taken_at: datetime
    label: str | None
    work_items: dict[str, WorkItem]
    ready_queue_order: list[str]
    artifacts: dict[str, Any]
    project_context: Any
    budget_state: BudgetState
