"""DispatchQueue — atomic task claiming for the executor.

Thin extraction from the former DispatcherStore. Only pop_ready
(FOR UPDATE SKIP LOCKED). All other task operations go through TaskCenter.
"""

from __future__ import annotations

from typing import Any, Callable  # used by _row_to_record

from sqlalchemy import text
from sqlalchemy.ext.asyncio import AsyncSession, async_sessionmaker

from team.persistence.task_record import TaskRecord

_RETURNING = (
    "id, team_run_id, agent_name, status, task,"
    " deps, scope_paths, scope_ltree,"
    " cascade_policy, parent_id, root_id, depth,"
    " pending_dep_count, retry_count, max_retries,"
    " agent_run_id, created_at, started_at,"
    " finished_at, failure_reason,"
    " blocker_id, pause_checkpoint, pause_verdict"
)


def _row_to_record(row: Any) -> TaskRecord:
    return TaskRecord(
        id=row.id, team_run_id=row.team_run_id, agent_name=row.agent_name,
        status=row.status, task=row.task,
        deps=list(row.deps) if row.deps else [],
        scope_paths=list(row.scope_paths) if row.scope_paths else [],
        scope_ltree=list(row.scope_ltree) if row.scope_ltree else [],
        cascade_policy=row.cascade_policy, parent_id=row.parent_id,
        root_id=row.root_id or "", depth=row.depth,
        pending_dep_count=row.pending_dep_count,
        retry_count=row.retry_count, max_retries=row.max_retries,
        agent_run_id=row.agent_run_id, created_at=row.created_at,
        started_at=row.started_at, finished_at=row.finished_at,
        failure_reason=row.failure_reason,
        blocker_id=getattr(row, 'blocker_id', None),
        pause_checkpoint=getattr(row, 'pause_checkpoint', None),
        pause_verdict=getattr(row, 'pause_verdict', None),
    )


class DispatchQueue:
    """Atomic task claiming. Two methods, same SQL, same atomicity."""

    def __init__(self, session_factory: async_sessionmaker[AsyncSession]) -> None:
        self._sf = session_factory

    async def pop_ready(
        self,
        run_id: str,
        blocker_guard: Callable[[TaskRecord], bool] | None = None,
    ) -> TaskRecord | None:
        """Atomically claim the next ready task via FOR UPDATE SKIP LOCKED."""
        async with self._sf() as db:
            rows = (await db.execute(text(f"""
                SELECT {_RETURNING}
                FROM tasks
                WHERE team_run_id = :run_id
                  AND status = 'ready'
                  AND pending_dep_count = 0
                ORDER BY depth, created_at
                LIMIT 32
                FOR UPDATE SKIP LOCKED
            """), {"run_id": run_id})).fetchall()
            selected: TaskRecord | None = None
            for row in rows:
                candidate = _row_to_record(row)
                if blocker_guard is not None and not blocker_guard(candidate):
                    continue
                selected = candidate
                break
            if selected is None:
                await db.commit()
                return None
            row = (await db.execute(text(f"""
                UPDATE tasks
                SET status = 'running', started_at = COALESCE(started_at, NOW())
                WHERE id = :task_id
                  AND team_run_id = :run_id
                  AND status = 'ready'
                RETURNING {_RETURNING}
            """), {"run_id": run_id, "task_id": selected.id})).fetchone()
            await db.commit()
            return _row_to_record(row) if row else None
