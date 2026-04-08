"""Checkpoint/rollback snapshots for a TeamRun."""

from __future__ import annotations

import copy
import uuid
from collections import deque
from dataclasses import dataclass, field
from datetime import datetime
from typing import Any

from team.types import BudgetState, CheckpointNotFound, WorkItem


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
    change_log_entries: list[Any]
    budget_state: BudgetState


class CheckpointStore:
    """In-memory ring buffer. Drops oldest when capacity is reached."""

    def __init__(self, max_checkpoints: int = 10) -> None:
        self._buf: deque[TeamRunCheckpoint] = deque(maxlen=max_checkpoints)
        self._seq = 0

    def next_sequence(self) -> int:
        self._seq += 1
        return self._seq

    def save(self, cp: TeamRunCheckpoint) -> None:
        self._buf.append(cp)

    def get(self, checkpoint_id: str) -> TeamRunCheckpoint | None:
        return next((cp for cp in self._buf if cp.id == checkpoint_id), None)

    def list(self) -> list[TeamRunCheckpoint]:
        return list(self._buf)

    def delete(self, checkpoint_id: str) -> bool:
        for cp in list(self._buf):
            if cp.id == checkpoint_id:
                self._buf.remove(cp)
                return True
        return False


def build_checkpoint(
    team_run_id: str,
    label: str | None,
    store: CheckpointStore,
    work_items: dict[str, WorkItem],
    ready_queue_order: list[str],
    artifacts: dict[str, Any],
    project_context: Any,
    change_log_entries: list[Any],
    budget_state: BudgetState,
) -> TeamRunCheckpoint:
    return TeamRunCheckpoint(
        id=str(uuid.uuid4()),
        team_run_id=team_run_id,
        sequence=store.next_sequence(),
        taken_at=datetime.utcnow(),
        label=label,
        work_items=copy.deepcopy(work_items),
        ready_queue_order=list(ready_queue_order),
        artifacts=copy.deepcopy(artifacts),
        project_context=copy.deepcopy(project_context),
        change_log_entries=copy.deepcopy(change_log_entries),
        budget_state=copy.deepcopy(budget_state),
    )
