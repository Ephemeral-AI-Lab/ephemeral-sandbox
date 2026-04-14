"""DispatchQueue — atomic task claiming for the executor.

Thin extraction from the former DispatcherStore. Only pop_ready
(FOR UPDATE SKIP LOCKED). All other task operations go through TaskCenter.
"""

from __future__ import annotations

from typing import Callable

from sqlalchemy import text
from sqlalchemy.ext.asyncio import AsyncSession, async_sessionmaker

from team.persistence.task_record import TASK_RETURNING, TaskRecord, row_to_record


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
                SELECT {TASK_RETURNING}
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
                candidate = row_to_record(row)
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
                RETURNING {TASK_RETURNING}
            """), {"run_id": run_id, "task_id": selected.id})).fetchone()
            await db.commit()
            return row_to_record(row) if row else None
