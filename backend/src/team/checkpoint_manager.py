"""CheckpointManager — run-state snapshot and rollback management.

Extracted from TaskCenter. Owns in-memory checkpoint ring buffer,
checkpoint persistence, and rollback/restore logic.
"""

from __future__ import annotations

import asyncio
import copy
import logging
import uuid
from collections import deque
from typing import Any, Callable

from team.models import BudgetState, Note, Task, _utcnow
from team.runtime.checkpoint import TeamRunCheckpoint

logger = logging.getLogger(__name__)


class CheckpointManager:
    """Manages run-state snapshots for recovery and rollback."""

    def __init__(
        self,
        team_run_id: str,
        max_checkpoints: int = 10,
    ) -> None:
        self._team_run_id = team_run_id
        self._checkpoints: deque[TeamRunCheckpoint] = deque(maxlen=max_checkpoints)
        self._checkpoint_seq = 0
        self._lock = asyncio.Lock()

    async def checkpoint(
        self,
        label: str | None,
        project_context: Any,
        tasks: dict[str, Task],
        ready_queue_order: list[str],
        notes: list[Note],
        budget_state: BudgetState,
        emit_checkpoint_cb: Callable[[str, str, int, str | None], None] | None = None,
    ) -> TeamRunCheckpoint:
        async with self._lock:
            self._checkpoint_seq += 1
            cp = TeamRunCheckpoint(
                id=str(uuid.uuid4()),
                team_run_id=self._team_run_id,
                sequence=self._checkpoint_seq,
                taken_at=_utcnow(),
                label=label,
                tasks=copy.deepcopy(tasks),
                ready_queue_order=list(ready_queue_order),
                notes=copy.deepcopy(notes),
                project_context=copy.deepcopy(project_context),
                budget_state=copy.deepcopy(budget_state),
            )
            self._checkpoints.append(cp)
            if emit_checkpoint_cb:
                emit_checkpoint_cb(self._team_run_id, cp.id, cp.sequence, label)
            return cp

    def list_checkpoints(self) -> list[TeamRunCheckpoint]:
        return list(self._checkpoints)

    def _get_checkpoint(self, checkpoint_id: str) -> TeamRunCheckpoint | None:
        return next((cp for cp in self._checkpoints if cp.id == checkpoint_id), None)

    async def rollback_to(
        self,
        checkpoint_id: str,
        project_context_setter: Callable[[Any], None],
        replace_run_tasks_fn: Callable[[list[Task]], Any],
        notes_restore_fn: Callable[[list[Note]], None],
        ready_queue_order_setter: Callable[[list[str]], None] | None = None,
    ) -> TeamRunCheckpoint | None:
        cp = self._get_checkpoint(checkpoint_id)
        if cp is None:
            return None
        await replace_run_tasks_fn(list(cp.tasks.values()))
        if ready_queue_order_setter is not None:
            ready_queue_order_setter(list(cp.ready_queue_order))
        notes_restore_fn(copy.deepcopy(cp.notes))
        project_context_setter(copy.deepcopy(cp.project_context))
        return cp

    async def prepare_for_resume(
        self,
        resume_snapshot: list[Task] | None,
        recover_running_fn: Callable[[], Any],
        replace_run_tasks_fn: Callable[[list[Task]], Any],
    ) -> None:
        if resume_snapshot is not None:
            await replace_run_tasks_fn(resume_snapshot)
        recovered = await recover_running_fn()
        if recovered:
            logger.info("Recovered %d running tasks to ready", len(recovered))
