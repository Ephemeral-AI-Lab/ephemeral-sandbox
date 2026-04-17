"""TaskStore — SQL persistence layer for tasks.

Extracted from TaskCenter to separate persistence from orchestration.
All raw SQL lives here; TaskCenter delegates to this class.
"""

from __future__ import annotations

import uuid
from collections import defaultdict, deque
from datetime import datetime, timezone

from sqlalchemy import delete, func, select, update
from sqlalchemy.ext.asyncio import AsyncSession, async_sessionmaker
from sqlalchemy.orm import aliased

from config.defaults import DEFAULT_MAX_RETRIES_PER_ITEM
from team.errors import GraphInvariantViolation
from team.models import TERMINAL_STATUSES, Task, TaskDefinition, TaskStatus, _utcnow
from team.persistence.ltree_utils import path_to_ltree
from team.persistence.task_graph import TaskGraph
from team.persistence.task_record import TaskRecord

_STATUSES_REQUIRING_DONE_DEPS = frozenset(
    {
        TaskStatus.READY,
        TaskStatus.RUNNING,
        TaskStatus.EXPANDED,
        TaskStatus.REPLANNING,
        TaskStatus.DONE,
    }
)


def record_to_task(rec: TaskRecord) -> Task:
    """Convert a TaskRecord ORM row to a domain Task."""
    return Task(
        id=rec.id,
        team_run_id=rec.team_run_id,
        agent_name=rec.agent_name,
        status=TaskStatus.of(rec.status),
        objective=rec.objective,
        description=rec.description or "",
        deps=list(rec.deps) if rec.deps else [],
        scope_paths=list(rec.scope_paths) if rec.scope_paths else [],
        parent_id=rec.parent_id,
        root_id=rec.root_id or "",
        depth=rec.depth or 0,
        retry_count=rec.retry_count or 0,
        max_retries=(
            rec.max_retries
            if rec.max_retries is not None
            else DEFAULT_MAX_RETRIES_PER_ITEM
        ),
        agent_run_id=rec.agent_run_id,
        created_at=rec.created_at or _utcnow(),
        started_at=rec.started_at,
        finished_at=rec.finished_at,
        failure_reason=rec.failure_reason,
        fired_by_task_id=getattr(rec, "fired_by_task_id", None),
    )


class TaskStore:
    """SQL persistence for tasks. Owns session_factory and team_run_id; delegates
    in-memory task graph / ready-queue bookkeeping to :class:`TaskGraph`.
    """

    def __init__(
        self,
        session_factory: async_sessionmaker[AsyncSession],
        team_run_id: str,
        max_retries_per_item: int = DEFAULT_MAX_RETRIES_PER_ITEM,
    ) -> None:
        self._sf = session_factory
        self._team_run_id = team_run_id
        self._max_retries_per_item = max_retries_per_item
        self._tg = TaskGraph()

    # ---- in-memory graph proxy --------------------------------------------

    @property
    def graph(self) -> dict[str, Task]:
        return self._tg.tasks

    @graph.setter
    def graph(self, value: dict[str, Task]) -> None:
        self._tg.tasks = value

    @property
    def ready_queue_order(self) -> list[str]:
        return list(self._tg.ready_order)

    @ready_queue_order.setter
    def ready_queue_order(self, value: list[str]) -> None:
        self._tg.ready_order = list(value)

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
            stmt = select(TaskRecord.id).where(TaskRecord.team_run_id == self._team_run_id)
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
                    TaskRecord.finished_at >= datetime.fromtimestamp(since, tz=timezone.utc)
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
            dependents = list(
                (
                    await db.execute(
                        select(TaskRecord)
                        .where(
                            TaskRecord.team_run_id == self._team_run_id,
                            TaskRecord.status == "pending",
                            TaskRecord.deps.any(task_id),
                        )
                        .with_for_update()
                    )
                )
                .scalars()
                .all()
            )
            promoted_ids: list[str] = []
            for dep in dependents:
                unsatisfied = await self._unsatisfied_dep_ids_sql(
                    db,
                    list(dep.deps or []),
                )
                dep.pending_dep_count = len(unsatisfied)
                if not unsatisfied:
                    dep.status = "ready"
                    promoted_ids.append(dep.id)
            await db.commit()
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
            non_successful_child = (
                select(1)
                .where(
                    sibling.parent_id == TaskRecord.id,
                    sibling.team_run_id == rid,
                    sibling.status != "done",
                )
                .exists()
            )
            stmt = select(TaskRecord.id).where(
                TaskRecord.id == parent_id_sub,
                TaskRecord.team_run_id == rid,
                TaskRecord.status == "expanded",
                ~non_successful_child,
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

    async def _done_ids_for_deps_sql(
        self,
        db: AsyncSession,
        dep_ids: set[str],
    ) -> set[str]:
        if not dep_ids:
            return set()
        rows = (
            (
                await db.execute(
                    select(TaskRecord.id).where(
                        TaskRecord.team_run_id == self._team_run_id,
                        TaskRecord.id.in_(dep_ids),
                        TaskRecord.status == "done",
                    )
                )
            )
            .scalars()
            .all()
        )
        return {str(row) for row in rows}

    async def _unsatisfied_dep_ids_sql(
        self,
        db: AsyncSession,
        dep_ids: list[str],
    ) -> list[str]:
        if not dep_ids:
            return []
        rows = (
            (
                await db.execute(
                    select(TaskRecord.id, TaskRecord.status).where(
                        TaskRecord.team_run_id == self._team_run_id,
                        TaskRecord.id.in_(set(dep_ids)),
                    )
                )
            )
            .all()
        )
        statuses = {str(row.id): str(row.status) for row in rows}
        unsatisfied: list[str] = []
        for dep_id in dep_ids:
            status = statuses.get(dep_id)
            if status == "done":
                continue
            unsatisfied.append(dep_id)
        return unsatisfied

    async def _assert_deps_satisfied_sql(
        self,
        db: AsyncSession,
        *,
        task_id: str,
        dep_ids: list[str],
        transition: str,
    ) -> None:
        unsatisfied = await self._unsatisfied_dep_ids_sql(
            db,
            dep_ids,
        )
        if unsatisfied:
            raise GraphInvariantViolation(
                f"task {task_id!r} cannot transition to {transition}; "
                f"unsatisfied dependencies: {', '.join(unsatisfied)}"
            )

    @staticmethod
    def _pending_dependency_rewrite_updates(
        rows: list[TaskRecord],
        *,
        old_dep_id: str,
        new_dep_ids: list[str],
    ) -> dict[str, list[str]]:
        invalid = [row for row in rows if row.status != "pending"]
        if invalid:
            details = ", ".join(f"{row.id}:{row.status}" for row in invalid)
            raise GraphInvariantViolation(
                "replan dependency invariant violated: "
                f"tasks depending on {old_dep_id!r} must be pending; found {details}"
            )

        updates: dict[str, list[str]] = {}
        for row in rows:
            deps = [dep_id for dep_id in row.deps if dep_id != old_dep_id]
            deps.extend(new_dep_ids)
            updates[row.id] = deps
        return updates

    async def _replace_dependency_sql(
        self,
        db: AsyncSession,
        *,
        old_dep_id: str,
        new_dep_ids: list[str],
    ) -> list[str]:
        rows = list(
            (
                await db.execute(
                    select(TaskRecord)
                    .where(
                        TaskRecord.team_run_id == self._team_run_id,
                        TaskRecord.deps.any(old_dep_id),
                    )
                    .order_by(TaskRecord.id)
                    .with_for_update()
                )
            )
            .scalars()
            .all()
        )
        updates = self._pending_dependency_rewrite_updates(
            rows,
            old_dep_id=old_dep_id,
            new_dep_ids=new_dep_ids,
        )
        if not updates:
            return []

        all_dep_ids = {dep for deps in updates.values() for dep in deps}
        done_ids = await self._done_ids_for_deps_sql(db, all_dep_ids)
        updated: list[str] = []
        for row in rows:
            seen: set[str] = set()
            unique_deps: list[str] = []
            for dep_id in updates[row.id]:
                if dep_id not in seen:
                    seen.add(dep_id)
                    unique_deps.append(dep_id)
            row.deps = unique_deps
            row.pending_dep_count = sum(1 for dep_id in unique_deps if dep_id not in done_ids)
            row.status = "pending"
            row.started_at = None
            row.agent_run_id = None
            updated.append(row.id)
        await db.flush()
        return updated

    async def _insert_plan_sql(
        self,
        db: AsyncSession,
        specs: list[TaskDefinition],
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
                    objective=spec.objective,
                    description=spec.description or "",
                    deps=list(spec.deps),
                    scope_paths=list(spec.scope_paths),
                    scope_ltree=[path_to_ltree(p) for p in spec.scope_paths],
                    parent_id=parent_id,
                    root_id=root_id or "",
                    depth=(parent_depth + 1) if parent_id else 0,
                    pending_dep_count=len(spec.deps),
                    max_retries=self._max_retries_per_item,
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
        specs: list[TaskDefinition],
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

    async def _cascade_recursive_sql(self, db: AsyncSession, root_task_id: str) -> list[str]:
        rid = self._team_run_id
        active_rows = list(
            (
                await db.execute(
                    select(TaskRecord).where(
                        TaskRecord.team_run_id == rid,
                        TaskRecord.status.notin_([s.value for s in TERMINAL_STATUSES]),
                    )
                )
            )
            .scalars()
            .all()
        )
        if not active_rows:
            return []

        records_by_id = {row.id: row for row in active_rows}
        children_by_parent: dict[str, list[str]] = defaultdict(list)
        dependents_by_dep: dict[str, list[str]] = defaultdict(list)
        for row in active_rows:
            if row.parent_id:
                children_by_parent[row.parent_id].append(row.id)
            for dep_id in row.deps or []:
                dependents_by_dep[dep_id].append(row.id)

        cancelled: set[str] = set()
        queue: deque[str] = deque([root_task_id])
        while queue:
            current = queue.popleft()
            for child_id in children_by_parent.get(current, []):
                if child_id not in cancelled:
                    cancelled.add(child_id)
                    queue.append(child_id)
            for dep_id in dependents_by_dep.get(current, []):
                record = records_by_id.get(dep_id)
                if record is None:
                    continue
                if dep_id not in cancelled:
                    cancelled.add(dep_id)
                    queue.append(dep_id)

        if not cancelled:
            return []

        stmt = (
            update(TaskRecord)
            .where(
                TaskRecord.team_run_id == rid,
                TaskRecord.id.in_(cancelled),
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

    async def fail_orphaned_replanning(self) -> int:
        """Force-fail all REPLANNING tasks whose replanner is terminal or missing."""
        rid = self._team_run_id
        async with self._sf() as db:
            # Find tasks stuck in REPLANNING
            rows = (
                (
                    await db.execute(
                        select(TaskRecord.id).where(
                            TaskRecord.team_run_id == rid,
                            TaskRecord.status == "replanning",
                        )
                    )
                )
                .scalars()
                .all()
            )
            if not rows:
                return 0
            for task_id in rows:
                await self._mark_terminal_sql(
                    db, str(task_id), "failed", "orphaned_replanning_timeout"
                )
                await self._cascade_recursive_sql(db, str(task_id))
            await db.commit()
        await self.refresh_graph()
        return len(rows)

    async def finalize_replanned_origin(self, replanner_task_id: str) -> str | None:
        """Mark the original REPLANNING task terminal after its replanner succeeds."""
        rid = self._team_run_id
        async with self._sf() as db:
            replanner = (
                await db.execute(
                    select(TaskRecord.fired_by_task_id).where(
                        TaskRecord.team_run_id == rid,
                        TaskRecord.id == replanner_task_id,
                    )
                )
            ).first()
            origin_id = (
                str(replanner.fired_by_task_id)
                if replanner and replanner.fired_by_task_id
                else None
            )
            if origin_id is None:
                return None
            result = await db.execute(
                update(TaskRecord)
                .where(
                    TaskRecord.team_run_id == rid,
                    TaskRecord.id == origin_id,
                    TaskRecord.status == "replanning",
                )
                .values(
                    status="failed",
                    finished_at=func.now(),
                    failure_reason=f"replanned_by:{replanner_task_id}",
                )
            )
            await db.commit()
        if not result.rowcount:
            return None
        await self.refresh_graph()
        return origin_id

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
                        TaskRecord.deps,
                    ).where(TaskRecord.id == task_id, TaskRecord.team_run_id == rid)
                )
            ).first()
            if rec is None or rec.status in ("done", "failed", "cancelled"):
                await db.commit()
                return warnings
            is_infra = reason.startswith(("worker_exception:", "runner_exception:"))
            if is_infra and rec.retry_count < rec.max_retries:
                await self._assert_deps_satisfied_sql(
                    db,
                    task_id=task_id,
                    dep_ids=list(rec.deps or []),
                    transition="ready",
                )
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
            await db.commit()
        await self.cascade_cancel_recursive(task_id)
        await self.refresh_graph()
        return warnings

    async def retry_task(self, task_id: str, max_retries: int) -> bool:
        rid = self._team_run_id
        async with self._sf() as db:
            rec = (
                await db.execute(
                    select(
                        TaskRecord.retry_count,
                        TaskRecord.deps,
                    ).where(
                        TaskRecord.id == task_id,
                        TaskRecord.team_run_id == rid,
                    )
                )
            ).first()
            if rec is None:
                return False
            if rec.retry_count >= max_retries:
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
                await self._assert_deps_satisfied_sql(
                    db,
                    task_id=task_id,
                    dep_ids=list(rec.deps or []),
                    transition="ready",
                )
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
                .values(status="cancelled", finished_at=func.now(), failure_reason=reason)
            )
            await db.commit()
            return result.rowcount

    async def _cancel_by_ids_sql(self, db: AsyncSession, task_ids: list[str], reason: str) -> int:
        if not task_ids:
            return 0
        result = await db.execute(
            update(TaskRecord)
            .where(
                TaskRecord.team_run_id == self._team_run_id,
                TaskRecord.id.in_(task_ids),
                TaskRecord.status.notin_([s.value for s in TERMINAL_STATUSES]),
            )
            .values(status="cancelled", finished_at=func.now(), failure_reason=reason)
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
        specs: list[TaskDefinition],
    ) -> tuple[int, list[TaskRecord]]:
        """Cancel requested graph nodes + cascade their descendants + insert new plan,
        all in a single transaction. If any step fails, the entire replan
        rolls back. Caller's in-memory graph is refreshed before return.
        """
        async with self._sf() as db:
            cancelled_count = await self._cancel_by_ids_sql(db, cancel_ids, cancel_reason)
            for cid in cancel_ids:
                await self._cascade_recursive_sql(db, cid)
            inserted: list[TaskRecord] = []
            specs_by_parent: dict[str | None, list[TaskDefinition]] = defaultdict(list)
            for spec in specs:
                specs_by_parent[spec.parent_id].append(spec)
            for parent_id, grouped_specs in specs_by_parent.items():
                parent_depth = 0
                parent_root_id: str | None = None
                if parent_id is not None:
                    parent_rec = (
                        await db.execute(
                            select(TaskRecord).where(
                                TaskRecord.team_run_id == self._team_run_id,
                                TaskRecord.id == parent_id,
                            )
                        )
                    ).scalar_one_or_none()
                    if parent_rec is None:
                        raise ValueError(f"replan parent '{parent_id}' not found")
                    parent_depth = parent_rec.depth or 0
                    parent_root_id = parent_rec.root_id or parent_rec.id
                inserted.extend(
                    await self._insert_plan_sql(
                        db,
                        grouped_specs,
                        parent_id,
                        parent_depth,
                        parent_root_id,
                    )
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
            if rec is not None:
                await self._assert_deps_satisfied_sql(
                    db,
                    task_id=rec.id,
                    dep_ids=list(rec.deps or []),
                    transition="running",
                )
            await db.commit()
        if rec is None:
            return None
        self._tg.upsert(record_to_task(rec))
        return rec

    async def recover_running(self) -> list[TaskRecord]:
        async with self._sf() as db:
            running = list(
                (
                    await db.execute(
                        select(TaskRecord)
                        .where(
                            TaskRecord.team_run_id == self._team_run_id,
                            TaskRecord.status == "running",
                        )
                        .with_for_update()
                    )
                )
                .scalars()
                .all()
            )
            for rec in running:
                await self._assert_deps_satisfied_sql(
                    db,
                    task_id=rec.id,
                    dep_ids=list(rec.deps or []),
                    transition="ready",
                )
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
        for task in tasks:
            if task.status not in _STATUSES_REQUIRING_DONE_DEPS:
                continue
            unsatisfied = [
                dep_id
                for dep_id in task.deps
                if dep_id not in done_ids
            ]
            if unsatisfied:
                raise GraphInvariantViolation(
                    f"snapshot task {task.id!r} is {task.status.value} with "
                    f"unsatisfied dependencies: {', '.join(unsatisfied)}"
                )
        async with self._sf() as db:
            await db.execute(delete(TaskRecord).where(TaskRecord.team_run_id == self._team_run_id))
            db.add_all(
                [
                    TaskRecord(
                        id=t.id,
                        team_run_id=self._team_run_id,
                        agent_name=t.agent_name,
                        status=t.status.value,
                        objective=t.objective,
                        deps=list(t.deps),
                        scope_paths=list(t.scope_paths),
                        scope_ltree=[path_to_ltree(p) for p in t.scope_paths],
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
                        fired_by_task_id=t.fired_by_task_id,
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
                        TaskRecord.status,
                        TaskRecord.fired_by_task_id,
                    ).where(TaskRecord.id == task_id, TaskRecord.team_run_id == rid)
                )
            ).first()
            if rec is None:
                raise RuntimeError(f"replan: {task_id} not found")
            replanner_id = str(uuid.uuid4())
            if rec.status != "replanning":
                await db.execute(
                    update(TaskRecord)
                    .where(TaskRecord.id == task_id, TaskRecord.team_run_id == rid)
                    .values(
                        status="replanning",
                        failure_reason=f"replan_requested: {reason}",
                    )
                )
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
                objective=task_text,
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
            await db.flush()
            await self._replace_dependency_sql(
                db,
                old_dep_id=task_id,
                new_dep_ids=[replanner_id],
            )
            await db.commit()
        await self.refresh_graph()
        return replanner
