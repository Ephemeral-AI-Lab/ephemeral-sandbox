"""DispatchQueue — atomic task claiming for the executor.

Thin extraction from the former DispatcherStore. Only pop_ready
(FOR UPDATE SKIP LOCKED). All other task operations go through TaskCenter.
"""

from __future__ import annotations

from sqlalchemy import func, select, update
from sqlalchemy.ext.asyncio import AsyncSession, async_sessionmaker

from team.errors import GraphInvariantViolation
from team.persistence.task_record import TaskRecord


class DispatchQueue:
    """Atomic task claiming. One method, same SQL, same atomicity."""

    def __init__(self, session_factory: async_sessionmaker[AsyncSession]) -> None:
        self._sf = session_factory

    async def pop_ready(
        self,
        run_id: str,
    ) -> TaskRecord | None:
        """Atomically claim the next ready task via FOR UPDATE SKIP LOCKED."""
        async with self._sf() as db:
            ready_id = (
                select(TaskRecord.id)
                .where(
                    TaskRecord.team_run_id == run_id,
                    TaskRecord.status == "ready",
                    TaskRecord.pending_dep_count == 0,
                )
                .order_by(TaskRecord.depth, TaskRecord.created_at)
                .limit(1)
                .with_for_update(skip_locked=True)
                .scalar_subquery()
            )
            stmt = (
                update(TaskRecord)
                .where(
                    TaskRecord.team_run_id == run_id,
                    TaskRecord.id == ready_id,
                )
                .values(
                    status="running",
                    started_at=func.coalesce(TaskRecord.started_at, func.now()),
                )
                .returning(TaskRecord)
                .execution_options(synchronize_session=False)
            )
            rec = (await db.execute(stmt)).scalar_one_or_none()
            if rec is not None and rec.deps:
                rows = (
                    (
                        await db.execute(
                            select(TaskRecord.id, TaskRecord.status).where(
                                TaskRecord.team_run_id == run_id,
                                TaskRecord.id.in_(set(rec.deps)),
                            )
                        )
                    )
                    .all()
                )
                statuses = {str(row.id): str(row.status) for row in rows}
                unsatisfied = [
                    dep_id
                    for dep_id in rec.deps
                    if statuses.get(dep_id) != "done"
                ]
                if unsatisfied:
                    raise GraphInvariantViolation(
                        f"task {rec.id!r} cannot transition to running; "
                        f"unsatisfied dependencies: {', '.join(unsatisfied)}"
                    )
            await db.commit()
            return rec
