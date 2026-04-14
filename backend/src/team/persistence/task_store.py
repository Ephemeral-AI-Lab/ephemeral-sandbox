"""TaskStore — SQL persistence layer for tasks.

Extracted from TaskCenter to separate persistence from orchestration.
All raw SQL lives here; TaskCenter delegates to this class.
"""

from __future__ import annotations

import uuid
from datetime import datetime, timezone

from sqlalchemy import and_, case, delete, func, or_, select, update
from sqlalchemy.ext.asyncio import AsyncSession, async_sessionmaker
from sqlalchemy.orm import aliased

from team.models import TERMINAL_STATUSES, Task, TaskSpec, TaskStatus, _utcnow
from team.persistence.ltree_utils import path_to_ltree
from team.persistence.task_graph import TaskGraph
from team.persistence.task_record import TaskRecord


def record_to_task(rec: TaskRecord) -> Task:
    """Convert a TaskRecord ORM row to a domain Task."""
    return Task(
        id=rec.id,
        team_run_id=rec.team_run_id,
        agent_name=rec.agent_name,
        status=TaskStatus(rec.status),
        task=rec.task,
        deps=list(rec.deps) if rec.deps else [],
        scope_paths=list(rec.scope_paths) if rec.scope_paths else [],
        cascade_policy=rec.cascade_policy or "cancel",
        parent_id=rec.parent_id,
        root_id=rec.root_id or "",
        depth=rec.depth or 0,
        retry_count=rec.retry_count or 0,
        max_retries=rec.max_retries or 2,
        agent_run_id=rec.agent_run_id,
        created_at=rec.created_at or _utcnow(),
        started_at=rec.started_at,
        finished_at=rec.finished_at,
        failure_reason=rec.failure_reason,
        blocker_id=getattr(rec, "blocker_id", None),
        fired_by_task_id=getattr(rec, "fired_by_task_id", None),
        pause_checkpoint=getattr(rec, "pause_checkpoint", None),
        pause_verdict=getattr(rec, "pause_verdict", None),
    )


class TaskStore:
    """SQL persistence for tasks. Owns session_factory and team_run_id; delegates
    in-memory task graph / ready-queue bookkeeping to :class:`TaskGraph`.
    """

    def __init__(
        self,
        session_factory: async_sessionmaker[AsyncSession],
        team_run_id: str,
    ) -> None:
        self._sf = session_factory
        self._team_run_id = team_run_id
        self._tg = TaskGraph()

    # ---- in-memory graph proxy --------------------------------------------

    @property
    def graph(self) -> dict[str, Task]:
        return self._tg.tasks

    @graph.setter
    def graph(self, value: dict[str, Task]) -> None:
        self._tg.tasks = value

    @property
    def _ready_order(self) -> list[str]:
        return self._tg.ready_order

    @_ready_order.setter
    def _ready_order(self, value: list[str]) -> None:
        self._tg.ready_order = value

    def get_task(self, task_id: str) -> Task | None:
        """Fast in-memory lookup — no DB call."""
        return self._tg.tasks.get(task_id)

    async def refresh_graph(self) -> dict[str, Task]:
        """Sync in-memory graph from DB. Returns the graph."""
        records = await self.get_all_tasks()
        self._tg.load(record_to_task(r) for r in records)
        return self._tg.tasks

    # ---- queries -------------------------------------------------------------

    async def get_record(self, task_id: str) -> TaskRecord | None:
        async with self._sf() as db:
            stmt = select(TaskRecord).where(
                TaskRecord.id == task_id,
                TaskRecord.team_run_id == self._team_run_id,
            )
            return (await db.execute(stmt)).scalar_one_or_none()

    async def get_all_tasks(self) -> list[TaskRecord]:
        async with self._sf() as db:
            stmt = (
                select(TaskRecord)
                .where(TaskRecord.team_run_id == self._team_run_id)
                .order_by(TaskRecord.depth, TaskRecord.created_at)
            )
            return list((await db.execute(stmt)).scalars().all())

    async def get_adjacency(self) -> dict[str, list[str]]:
        async with self._sf() as db:
            stmt = select(TaskRecord.id, TaskRecord.deps).where(
                TaskRecord.team_run_id == self._team_run_id
            )
            rows = (await db.execute(stmt)).all()
            return {r.id: list(r.deps) if r.deps else [] for r in rows}

    async def get_statuses(self) -> dict[str, str]:
        async with self._sf() as db:
            stmt = select(TaskRecord.id, TaskRecord.status).where(
                TaskRecord.team_run_id == self._team_run_id
            )
            rows = (await db.execute(stmt)).all()
            return {r.id: r.status for r in rows}

    async def get_task_ids(self) -> set[str]:
        async with self._sf() as db:
            stmt = select(TaskRecord.id).where(
                TaskRecord.team_run_id == self._team_run_id
            )
            return {str(tid) for tid in (await db.execute(stmt)).scalars().all()}

    async def get_done_sibling_ids(
        self,
        *,
        task_id: str,
        parent_id: str | None,
        since: float | None = None,
    ) -> list[str]:
        async with self._sf() as db:
            stmt = (
                select(TaskRecord.id)
                .where(
                    TaskRecord.team_run_id == self._team_run_id,
                    TaskRecord.parent_id.is_not_distinct_from(parent_id),
                    TaskRecord.id != task_id,
                    TaskRecord.status == "done",
                )
                .order_by(TaskRecord.finished_at, TaskRecord.created_at)
            )
            if since is not None:
                stmt = stmt.where(
                    TaskRecord.finished_at
                    >= datetime.fromtimestamp(since, tz=timezone.utc)
                )
            return [str(tid) for tid in (await db.execute(stmt)).scalars().all()]

    async def all_terminal(self) -> bool:
        async with self._sf() as db:
            stmt = select(func.count()).where(
                TaskRecord.team_run_id == self._team_run_id,
                TaskRecord.status.notin_(("done", "failed", "cancelled")),
            )
            return (await db.execute(stmt)).scalar() == 0

    async def sibling_stats(self, parent_id: str | None) -> dict[str, int]:
        async with self._sf() as db:
            stmt = (
                select(
                    TaskRecord.status,
                    func.count().label("cnt"),
                    func.sum(TaskRecord.retry_count).label("retries"),
                )
                .where(
                    TaskRecord.team_run_id == self._team_run_id,
                    TaskRecord.parent_id.is_not_distinct_from(parent_id),
                )
                .group_by(TaskRecord.status)
            )
            result = await db.execute(stmt)
            stats: dict[str, int] = {
                "done": 0,
                "failed": 0,
                "cancelled": 0,
                "running": 0,
                "pending": 0,
                "ready": 0,
                "expanded": 0,
                "retry_total": 0,
            }
            for row in result.all():
                stats[row.status] = row.cnt
                stats["retry_total"] += int(row.retries or 0)
            return stats

    async def sibling_subtree_ids(self, parent_id: str | None) -> list[str]:
        rid = self._team_run_id
        subtree = (
            select(TaskRecord.id)
            .where(
                TaskRecord.team_run_id == rid,
                TaskRecord.parent_id.is_not_distinct_from(parent_id),
            )
            .cte("subtree", recursive=True)
        )
        child = aliased(TaskRecord, name="child")
        subtree = subtree.union_all(
            select(child.id)
            .join(subtree, child.parent_id == subtree.c.id)
            .where(child.team_run_id == rid)
        )
        async with self._sf() as db:
            rows = (await db.execute(select(subtree.c.id))).all()
        return [str(r.id) for r in rows]

    async def get_siblings_and_descendants(self, initiating_task_id: str) -> list[TaskRecord]:
        """Return all siblings of the initiating task plus their entire subtrees.

        Siblings share the same parent_id. Descendants are found via recursive
        CTE on parent_id. The initiating task itself is excluded.
        """
        rid = self._team_run_id
        initiator = aliased(TaskRecord, name="initiator")
        parent_of_initiator = (
            select(initiator.parent_id)
            .where(initiator.id == initiating_task_id, initiator.team_run_id == rid)
            .scalar_subquery()
        )
        subtree = (
            select(TaskRecord.id)
            .where(
                TaskRecord.team_run_id == rid,
                TaskRecord.parent_id.is_not_distinct_from(parent_of_initiator),
                TaskRecord.id != initiating_task_id,
            )
            .cte("subtree", recursive=True)
        )
        child = aliased(TaskRecord, name="child")
        subtree = subtree.union_all(
            select(child.id)
            .join(subtree, child.parent_id == subtree.c.id)
            .where(child.team_run_id == rid)
        )
        stmt = (
            select(TaskRecord)
            .where(
                TaskRecord.team_run_id == rid,
                TaskRecord.id.in_(select(subtree.c.id)),
            )
            .order_by(TaskRecord.depth, TaskRecord.created_at)
        )
        async with self._sf() as db:
            return list((await db.execute(stmt)).scalars().all())

    # ---- mutations -----------------------------------------------------------

    async def mark_done(self, task_id: str) -> list[str]:
        async with self._sf() as db:
            await db.execute(
                update(TaskRecord)
                .where(
                    TaskRecord.id == task_id,
                    TaskRecord.team_run_id == self._team_run_id,
                )
                .values(status="done", finished_at=func.now())
            )
            promote_stmt = (
                update(TaskRecord)
                .where(
                    TaskRecord.team_run_id == self._team_run_id,
                    TaskRecord.status == "pending",
                    TaskRecord.deps.any(task_id),
                    TaskRecord.pending_dep_count > 0,
                )
                .values(
                    pending_dep_count=TaskRecord.pending_dep_count - 1,
                    status=case(
                        ((TaskRecord.pending_dep_count - 1) == 0, "ready"),
                        else_=TaskRecord.status,
                    ),
                )
                .returning(TaskRecord.id, TaskRecord.pending_dep_count)
                .execution_options(synchronize_session=False)
            )
            promoted_rows = (await db.execute(promote_stmt)).all()
            await db.commit()
            promoted_ids = [r.id for r in promoted_rows if r.pending_dep_count == 0]
        self._tg.mark_done(task_id, promoted_ids)
        return promoted_ids

    async def mark_expanded(self, task_id: str) -> None:
        async with self._sf() as db:
            await db.execute(
                update(TaskRecord)
                .where(
                    TaskRecord.id == task_id,
                    TaskRecord.team_run_id == self._team_run_id,
                )
                .values(status="expanded")
            )
            await db.commit()
        self._tg.mark_expanded(task_id)

    async def maybe_promote_expanded_parent(self, child_id: str) -> list[str]:
        rid = self._team_run_id
        promoted_all: list[str] = []
        current = child_id
        while True:
            child = aliased(TaskRecord, name="child")
            parent_id_sub = (
                select(child.parent_id)
                .where(child.id == current, child.team_run_id == rid)
                .scalar_subquery()
            )
            sibling = aliased(TaskRecord, name="sibling")
            unfinished_sibling = (
                select(1)
                .where(
                    sibling.parent_id == TaskRecord.id,
                    sibling.team_run_id == rid,
                    sibling.status.notin_(("done", "failed", "cancelled")),
                )
                .exists()
            )
            stmt = select(TaskRecord.id).where(
                TaskRecord.id == parent_id_sub,
                TaskRecord.team_run_id == rid,
                TaskRecord.status == "expanded",
                ~unfinished_sibling,
            )
            async with self._sf() as db:
                row = (await db.execute(stmt)).first()
            if row is None:
                break
            pid = str(row.id)
            promoted = await self.mark_done(pid)
            promoted_all.append(pid)
            promoted_all.extend(promoted)
            current = pid
        return promoted_all

    async def _mark_terminal_sql(
        self, db: AsyncSession, task_id: str, status: str, reason: str
    ) -> None:
        await db.execute(
            update(TaskRecord)
            .where(
                TaskRecord.id == task_id,
                TaskRecord.team_run_id == self._team_run_id,
            )
            .values(status=status, finished_at=func.now(), failure_reason=reason)
        )

    async def mark_terminal(self, task_id: str, status: str, reason: str) -> None:
        async with self._sf() as db:
            await self._mark_terminal_sql(db, task_id, status, reason)
            await db.commit()
        self._tg.mark_terminal(task_id, status, reason)

    async def _insert_plan_sql(
        self,
        db: AsyncSession,
        specs: list[TaskSpec],
        parent_id: str | None,
        parent_depth: int,
        parent_root_id: str | None,
    ) -> list[TaskRecord]:
        if not specs:
            return []
        records: list[TaskRecord] = []
        for spec in specs:
            status = "ready" if not spec.deps else "pending"
            root_id = parent_root_id if parent_id else spec.id
            records.append(
                TaskRecord(
                    id=spec.id,
                    team_run_id=self._team_run_id,
                    agent_name=spec.agent,
                    status=status,
                    task=spec.task,
                    deps=list(spec.deps),
                    scope_paths=list(spec.scope_paths),
                    scope_ltree=[path_to_ltree(p) for p in spec.scope_paths],
                    parent_id=parent_id,
                    root_id=root_id or "",
                    depth=(parent_depth + 1) if parent_id else 0,
                    pending_dep_count=len(spec.deps),
                )
            )
        db.add_all(records)
        await db.flush()
        # Backfill pending_dep_count for newly-inserted tasks whose deps include
        # already-completed tasks. Only freshly-inserted records can need this
        # adjustment — pre-existing pending rows are already correct.
        if any(rec.status == "pending" and rec.deps for rec in records):
            done_ids: set[str] = set(
                (
                    await db.execute(
                        select(TaskRecord.id).where(
                            TaskRecord.team_run_id == self._team_run_id,
                            TaskRecord.status == "done",
                        )
                    )
                )
                .scalars()
                .all()
            )
            if done_ids:
                for rec in records:
                    if rec.status != "pending" or not rec.deps:
                        continue
                    satisfied = sum(1 for d in rec.deps if d in done_ids)
                    if satisfied == 0:
                        continue
                    rec.pending_dep_count = rec.pending_dep_count - satisfied
                    if rec.pending_dep_count == 0:
                        rec.status = "ready"
                await db.flush()
        inserted_ids = [record.id for record in records]
        stmt = (
            select(TaskRecord)
            .where(
                TaskRecord.team_run_id == self._team_run_id,
                TaskRecord.id.in_(inserted_ids),
            )
            .order_by(TaskRecord.depth, TaskRecord.created_at)
        )
        recs = list((await db.execute(stmt)).scalars().all())
        return recs

    async def insert_plan(
        self,
        specs: list[TaskSpec],
        parent_id: str | None = None,
        parent_depth: int = 0,
        parent_root_id: str | None = None,
    ) -> list[TaskRecord]:
        async with self._sf() as db:
            result_records = await self._insert_plan_sql(
                db, specs, parent_id, parent_depth, parent_root_id
            )
            await db.commit()
        self._tg.insert_tasks(record_to_task(rec) for rec in result_records)
        return result_records

    async def _cascade_recursive_sql(
        self, db: AsyncSession, root_task_id: str
    ) -> list[str]:
        rid = self._team_run_id
        dep_chain = (
            select(TaskRecord.id)
            .where(TaskRecord.team_run_id == rid, TaskRecord.id == root_task_id)
            .cte("dep_chain", recursive=True)
        )
        descendant = aliased(TaskRecord, name="descendant")
        dep_chain = dep_chain.union_all(
            select(descendant.id)
            .join(
                dep_chain,
                or_(
                    and_(
                        descendant.deps.any(dep_chain.c.id),
                        descendant.cascade_policy != "continue",
                    ),
                    descendant.parent_id == dep_chain.c.id,
                ),
            )
            .where(
                descendant.team_run_id == rid,
                descendant.status.in_(("pending", "ready", "expanded")),
            )
        )
        cascade_ids = (
            select(dep_chain.c.id)
            .where(dep_chain.c.id != root_task_id)
            .distinct()
        )
        stmt = (
            update(TaskRecord)
            .where(
                TaskRecord.team_run_id == rid,
                TaskRecord.id.in_(cascade_ids),
            )
            .values(
                status="cancelled",
                finished_at=func.now(),
                failure_reason=f"cascaded from {root_task_id}",
            )
            .returning(TaskRecord.id)
            .execution_options(synchronize_session=False)
        )
        result = await db.execute(stmt)
        return [r.id for r in result.fetchall()]

    async def cascade_cancel_recursive(self, root_task_id: str) -> list[str]:
        async with self._sf() as db:
            cancelled = await self._cascade_recursive_sql(db, root_task_id)
            await db.commit()
        self._tg.mark_cancelled(cancelled)
        return cancelled

    async def fail_with_cascade(self, task_id: str, reason: str) -> list[str]:
        """Mark task failed AND cascade-cancel descendants in one transaction.

        Returns the list of cascaded descendant ids. Caller should refresh
        the graph to pick up the in-memory state changes.
        """
        async with self._sf() as db:
            await self._mark_terminal_sql(db, task_id, "failed", reason)
            cancelled = await self._cascade_recursive_sql(db, task_id)
            await db.commit()
        await self.refresh_graph()
        return cancelled

    async def rewire_dependents(
        self, original_task_id: str, new_dep_ids: list[str]
    ) -> list[str]:
        """Replace *original_task_id* in every dependent's deps with *new_dep_ids*.

        After rewiring, marks the original task FAILED (terminal) — safe because
        nothing depends on it anymore.  Returns IDs of dependents that were
        rewired (and any that got promoted to READY).
        """
        rid = self._team_run_id
        rewired: list[str] = []
        promoted: list[str] = []
        async with self._sf() as db:
            # 1. Find all non-terminal tasks that depend on the original
            rows = (
                await db.execute(
                    select(TaskRecord)
                    .where(
                        TaskRecord.team_run_id == rid,
                        TaskRecord.deps.any(original_task_id),
                        TaskRecord.status.notin_(
                            [s.value for s in TERMINAL_STATUSES]
                        ),
                    )
                )
            ).scalars().all()

            if not rows:
                # No dependents — just mark original failed
                await self._mark_terminal_sql(db, original_task_id, "failed",
                                              "replan_completed_no_dependents")
                await db.commit()
                await self.refresh_graph()
                return []

            # 2. Collect all unique dep IDs we need statuses for
            all_dep_ids: set[str] = set()
            for dep_rec in rows:
                new_deps = [d for d in dep_rec.deps if d != original_task_id] + list(new_dep_ids)
                all_dep_ids.update(new_deps)

            # Bulk-fetch statuses
            done_ids: set[str] = set()
            if all_dep_ids:
                done_rows = (
                    await db.execute(
                        select(TaskRecord.id)
                        .where(
                            TaskRecord.team_run_id == rid,
                            TaskRecord.id.in_(all_dep_ids),
                            TaskRecord.status == "done",
                        )
                    )
                ).scalars().all()
                done_ids = set(done_rows)

            # 3. Rewire each dependent
            for dep_rec in rows:
                new_deps = [d for d in dep_rec.deps if d != original_task_id] + list(new_dep_ids)
                # Deduplicate while preserving order
                seen: set[str] = set()
                unique_deps: list[str] = []
                for d in new_deps:
                    if d not in seen:
                        seen.add(d)
                        unique_deps.append(d)
                dep_rec.deps = unique_deps
                pending = sum(1 for d in unique_deps if d not in done_ids)
                dep_rec.pending_dep_count = pending
                if pending == 0 and dep_rec.status == "pending":
                    dep_rec.status = "ready"
                    promoted.append(dep_rec.id)
                rewired.append(dep_rec.id)

            # 4. Mark original as FAILED (nothing depends on it now)
            await self._mark_terminal_sql(db, original_task_id, "failed",
                                          "replan_completed_deps_rewired")
            await db.commit()

        await self.refresh_graph()
        return rewired + promoted

    async def fail_task(self, task_id: str, reason: str) -> list[tuple[str, str]]:
        warnings: list[tuple[str, str]] = []
        rid = self._team_run_id
        async with self._sf() as db:
            rec = (
                await db.execute(
                    select(
                        TaskRecord.id,
                        TaskRecord.status,
                        TaskRecord.retry_count,
                        TaskRecord.max_retries,
                    ).where(TaskRecord.id == task_id, TaskRecord.team_run_id == rid)
                )
            ).first()
            if rec is None or rec.status in ("done", "failed", "cancelled"):
                await db.commit()
                return warnings
            is_infra = reason.startswith(("worker_exception:", "runner_exception:"))
            if rec.retry_count < rec.max_retries:
                should_retry = is_infra
                if not should_retry:
                    should_retry = (
                        await db.execute(
                            select(
                                select(TaskRecord.id)
                                .where(
                                    TaskRecord.team_run_id == rid,
                                    TaskRecord.deps.any(task_id),
                                    TaskRecord.cascade_policy == "retry_first",
                                    TaskRecord.status.notin_(
                                        ("done", "failed", "cancelled")
                                    ),
                                )
                                .exists()
                            )
                        )
                    ).scalar()
                if should_retry:
                    await db.execute(
                        update(TaskRecord)
                        .where(
                            TaskRecord.id == task_id,
                            TaskRecord.team_run_id == rid,
                        )
                        .values(
                            status="ready",
                            retry_count=TaskRecord.retry_count + 1,
                            agent_run_id=None,
                            started_at=None,
                            finished_at=None,
                            failure_reason=None,
                        )
                    )
                    await db.commit()
                    self._tg.set_ready_status(task_id)
                    return warnings
            await db.execute(
                update(TaskRecord)
                .where(TaskRecord.id == task_id, TaskRecord.team_run_id == rid)
                .values(status="failed", finished_at=func.now(), failure_reason=reason)
            )
            cont = (
                await db.execute(
                    select(TaskRecord.id).where(
                        TaskRecord.team_run_id == rid,
                        TaskRecord.deps.any(task_id),
                        TaskRecord.cascade_policy == "continue",
                        TaskRecord.status.notin_(("done", "failed", "cancelled")),
                    )
                )
            ).scalars().all()
            for dep_id in cont:
                warnings.append(
                    (
                        dep_id,
                        f"Warning: dependency {task_id} failed: {reason}. Proceed with caution.",
                    )
                )
            await db.commit()
        self._tg.mark_failed(task_id, reason)
        await self.cascade_cancel_recursive(task_id)
        return warnings

    async def retry_task(self, task_id: str, max_retries: int) -> bool:
        rid = self._team_run_id
        async with self._sf() as db:
            retry_count = (
                await db.execute(
                    select(TaskRecord.retry_count).where(
                        TaskRecord.id == task_id, TaskRecord.team_run_id == rid
                    )
                )
            ).scalar_one_or_none()
            if retry_count is None:
                return False
            if retry_count >= max_retries:
                await db.execute(
                    update(TaskRecord)
                    .where(TaskRecord.id == task_id, TaskRecord.team_run_id == rid)
                    .values(
                        status="failed",
                        finished_at=func.now(),
                        failure_reason="retry_exhausted",
                    )
                )
                await db.commit()
            else:
                await db.execute(
                    update(TaskRecord)
                    .where(TaskRecord.id == task_id, TaskRecord.team_run_id == rid)
                    .values(
                        status="ready",
                        retry_count=TaskRecord.retry_count + 1,
                        agent_run_id=None,
                        started_at=None,
                        finished_at=None,
                        failure_reason=None,
                    )
                )
                await db.commit()
                self._tg.requeue_ready(task_id)
                return True
        self._tg.mark_failed(task_id)
        await self.cascade_cancel_recursive(task_id)
        return False

    async def cancel_all_pending(self) -> int:
        async with self._sf() as db:
            result = await db.execute(
                update(TaskRecord)
                .where(
                    TaskRecord.team_run_id == self._team_run_id,
                    TaskRecord.status.in_(("pending", "ready", "expanded")),
                )
                .values(
                    status="cancelled",
                    finished_at=func.now(),
                    failure_reason="team_run cancelled",
                )
            )
            await db.commit()
            return result.rowcount

    async def cancel_all_running(self, reason: str) -> int:
        async with self._sf() as db:
            result = await db.execute(
                update(TaskRecord)
                .where(
                    TaskRecord.team_run_id == self._team_run_id,
                    TaskRecord.status == "running",
                )
                .values(
                    status="cancelled", finished_at=func.now(), failure_reason=reason
                )
            )
            await db.commit()
            return result.rowcount

    async def pause_running_task(
        self,
        task_id: str,
        blocker_id: str,
        checkpoint: str,
        verdict: str,
    ) -> bool:
        async with self._sf() as db:
            result = await db.execute(
                update(TaskRecord)
                .where(
                    TaskRecord.id == task_id,
                    TaskRecord.team_run_id == self._team_run_id,
                    TaskRecord.status == "running",
                )
                .values(
                    status="paused",
                    blocker_id=blocker_id,
                    pause_checkpoint=checkpoint,
                    pause_verdict=verdict,
                )
            )
            await db.commit()
            if result.rowcount > 0:
                self._tg.pause(task_id, blocker_id, checkpoint, verdict)
            return result.rowcount > 0

    async def resume_paused_tasks(self, blocker_id: str) -> int:
        async with self._sf() as db:
            result = await db.execute(
                update(TaskRecord)
                .where(
                    TaskRecord.team_run_id == self._team_run_id,
                    TaskRecord.blocker_id == blocker_id,
                    TaskRecord.status == "paused",
                )
                .values(
                    status="ready",
                    blocker_id=None,
                    agent_run_id=None,
                    started_at=None,
                    finished_at=None,
                    failure_reason=None,
                )
            )
            await db.commit()
            return result.rowcount

    async def cancel_paused_tasks(self, blocker_id: str) -> int:
        async with self._sf() as db:
            result = await db.execute(
                update(TaskRecord)
                .where(
                    TaskRecord.team_run_id == self._team_run_id,
                    TaskRecord.blocker_id == blocker_id,
                    TaskRecord.status == "paused",
                )
                .values(
                    status="cancelled",
                    finished_at=func.now(),
                    failure_reason="blocker_failed",
                    blocker_id=None,
                )
            )
            await db.commit()
            return result.rowcount

    async def _cancel_by_ids_sql(
        self, db: AsyncSession, task_ids: list[str], reason: str
    ) -> int:
        if not task_ids:
            return 0
        result = await db.execute(
            update(TaskRecord)
            .where(
                TaskRecord.team_run_id == self._team_run_id,
                TaskRecord.id.in_(task_ids),
                TaskRecord.status.in_(("pending", "ready", "expanded")),
            )
            .values(
                status="cancelled", finished_at=func.now(), failure_reason=reason
            )
        )
        return result.rowcount or 0

    async def cancel_by_ids(self, task_ids: list[str], reason: str) -> int:
        if not task_ids:
            return 0
        async with self._sf() as db:
            count = await self._cancel_by_ids_sql(db, task_ids, reason)
            await db.commit()
            return count

    async def apply_replan_atomic(
        self,
        *,
        cancel_ids: list[str],
        cancel_reason: str,
        specs: list[TaskSpec],
        parent_id: str | None,
        parent_depth: int,
        parent_root_id: str | None,
    ) -> tuple[int, list[TaskRecord]]:
        """Cancel sibling ids + cascade their descendants + insert new plan,
        all in a single transaction. If any step fails, the entire replan
        rolls back. Caller's in-memory graph is refreshed before return.
        """
        async with self._sf() as db:
            cancelled_count = await self._cancel_by_ids_sql(db, cancel_ids, cancel_reason)
            for cid in cancel_ids:
                await self._cascade_recursive_sql(db, cid)
            inserted = await self._insert_plan_sql(
                db, specs, parent_id, parent_depth, parent_root_id
            )
            await db.commit()
        await self.refresh_graph()
        return cancelled_count, inserted

    async def mark_running_sql(self, task_id: str, agent_run_id: str) -> TaskRecord | None:
        async with self._sf() as db:
            stmt = (
                update(TaskRecord)
                .where(
                    TaskRecord.id == task_id,
                    TaskRecord.team_run_id == self._team_run_id,
                    TaskRecord.status == "running",
                )
                .values(
                    agent_run_id=agent_run_id,
                    started_at=func.coalesce(TaskRecord.started_at, func.now()),
                )
                .returning(TaskRecord)
                .execution_options(synchronize_session=False)
            )
            rec = (await db.execute(stmt)).scalar_one_or_none()
            await db.commit()
        if rec is None:
            return None
        self._tg.upsert(record_to_task(rec))
        return rec

    async def recover_running(self) -> list[TaskRecord]:
        async with self._sf() as db:
            stmt = (
                update(TaskRecord)
                .where(
                    TaskRecord.team_run_id == self._team_run_id,
                    TaskRecord.status == "running",
                )
                .values(status="ready", started_at=None, agent_run_id=None)
                .returning(TaskRecord)
                .execution_options(synchronize_session=False)
            )
            recs = list((await db.execute(stmt)).scalars().all())
            await db.commit()
            self._tg.recover_running(record_to_task(rec) for rec in recs)
            return recs

    async def replace_run_tasks(self, tasks: list[Task]) -> None:
        done_ids = {t.id for t in tasks if t.status == TaskStatus.DONE}
        async with self._sf() as db:
            await db.execute(
                delete(TaskRecord).where(TaskRecord.team_run_id == self._team_run_id)
            )
            db.add_all(
                [
                    TaskRecord(
                        id=t.id,
                        team_run_id=self._team_run_id,
                        agent_name=t.agent_name,
                        status=t.status.value,
                        task=t.task,
                        deps=list(t.deps),
                        scope_paths=list(t.scope_paths),
                        scope_ltree=[path_to_ltree(p) for p in t.scope_paths],
                        cascade_policy=t.cascade_policy,
                        parent_id=t.parent_id,
                        root_id=t.root_id or "",
                        depth=t.depth,
                        pending_dep_count=len([d for d in t.deps if d not in done_ids]),
                        retry_count=t.retry_count,
                        max_retries=t.max_retries,
                        agent_run_id=t.agent_run_id,
                        created_at=t.created_at,
                        started_at=t.started_at,
                        finished_at=t.finished_at,
                        failure_reason=t.failure_reason,
                        blocker_id=t.blocker_id,
                        pause_checkpoint=t.pause_checkpoint,
                        pause_verdict=t.pause_verdict,
                    )
                    for t in tasks
                ]
            )
            await db.commit()
        self._tg.load(tasks)

    async def request_replan(
        self,
        task_id: str,
        reason: str,
        suggestion: str | None,
        replanner_agent: str,
    ) -> TaskRecord:
        rid = self._team_run_id
        async with self._sf() as db:
            rec = (
                await db.execute(
                    select(
                        TaskRecord.id,
                        TaskRecord.parent_id,
                        TaskRecord.root_id,
                        TaskRecord.depth,
                        TaskRecord.agent_name,
                        TaskRecord.scope_paths,
                    ).where(TaskRecord.id == task_id, TaskRecord.team_run_id == rid)
                )
            ).first()
            if rec is None:
                raise RuntimeError(f"replan: {task_id} not found")
            if rec.status != "replanning":
                await db.execute(
                    update(TaskRecord)
                    .where(TaskRecord.id == task_id, TaskRecord.team_run_id == rid)
                    .values(
                        status="replanning",
                        failure_reason=f"replan_requested: {reason}",
                    )
                )
            await db.commit()
        async with self._sf() as db:
            replanner_id = str(uuid.uuid4())
            task_text = f"Replan: {rec.agent_name} failed on task {task_id}: {reason}"
            if suggestion:
                task_text += f"\nSuggestion: {suggestion}"
            scope_paths = list(rec.scope_paths) if rec.scope_paths else []
            # fired_by_task_id always points to the root original, not
            # an intermediate replanner, so chains stay one-hop deep.
            root_origin = getattr(rec, "fired_by_task_id", None) or task_id
            replanner = TaskRecord(
                id=replanner_id,
                team_run_id=rid,
                agent_name=replanner_agent,
                task=task_text,
                status="ready",
                deps=[],
                scope_paths=scope_paths,
                scope_ltree=[path_to_ltree(p) for p in scope_paths],
                parent_id=rec.parent_id,
                root_id=rec.root_id or "",
                depth=rec.depth or 0,
                pending_dep_count=0,
                fired_by_task_id=root_origin,
            )
            db.add(replanner)
            await db.commit()
        self._tg.apply_replan(
            failed_task_id=task_id,
            reason=reason,
            replanner_task=record_to_task(replanner),
        )
        return replanner
