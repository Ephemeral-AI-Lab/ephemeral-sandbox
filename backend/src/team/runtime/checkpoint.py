"""Checkpoint snapshot dataclass for a TeamRun."""

from __future__ import annotations

from dataclasses import dataclass
from datetime import datetime
from typing import Any

from team.models import BudgetState, Note, Task


@dataclass
class TeamRunCheckpoint:
    id: str
    team_run_id: str
    sequence: int
    taken_at: datetime
    label: str | None
    tasks: dict[str, Task]
    ready_queue_order: list[str]
    notes: list[Note]
    project_context: Any
    budget_state: BudgetState
