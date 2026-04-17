"""TaskStore — SQL persistence layer for tasks.

Owns session lifecycle + in-memory ``TaskGraph`` bookkeeping. All SQLAlchemy
queries live in :mod:`team.persistence.task_queries`.
"""

from __future__ import annotations

import uuid
from collections import defaultdict

from sqlalchemy.ext.asyncio import AsyncSession, async_sessionmaker

from team.errors import GraphInvariantViolation
from team.models import TERMINAL_STATUSES, Task, TaskDefinition, TaskStatus, _utcnow
from team.persistence import task_queries as q
from team.persistence.ltree_utils import path_to_ltree
from team.persistence.task_graph import TaskGraph
from team.persistence.task_record import TaskRecord

_STATUSES_REQUIRING_DONE_DEPS = frozenset(
    {
        TaskStatus.READY,
        TaskStatus.RUNNING,
        TaskStatus.EXPANDED,
        TaskStatus.REQUEST_REPLAN,
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
        agent_run_id=rec.agent_run_id,
        created_at=rec.created_at or _utcnow(),
        started_at=rec.started_at,
        finished_at=rec.finished_at,
        failure_reason=rec.failure_reason,
        fired_by_task_id=getattr(rec, "fired_by_task_id", None),
    )


class TaskStore:
    """SQL persistence for tasks. Owns session_factory and team_run_id; delegates
    raw queries to :mod:`task_queries` and in-memory graph / ready-queue
    bookkeeping to :class:`TaskGraph`.
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
    def ready_queue_order(self) -> list[str]:
        return list(self._tg.ready_order)

    @ready_queue_order.setter
    def ready_queue_order(self, value: list[str]) -> None:
        self._tg.ready_order = list(value)

    def get_task(self, task_id: str) -> Task | None:
        """Fast in-memory lookup — no DB call."""
        return self._tg.tasks.get(task_id)

    def children_of(self, parent_id: str) -> list[Task]:
        """All in-memory children of ``parent_id``."""
        return self._tg.children_of(parent_id)

    def detached_children(self, parent_id: str) -> list[Task]:
        """Children of ``parent_id`` whose status is failed or cancelled."""
        return self._tg.detached_children(parent_id)

    def live_children(self, parent_id: str) -> list[Task]:
        """Children of ``parent_id`` that are not detached."""
        return self._tg.live_children(parent_id)

    async def refresh_graph(self) -> dict[str, Task]:
        """Sync in-memory graph from DB. Returns the graph."""
        records = await self.get_all_tasks()
        self._tg.load(record_to_task(r) for r in records)
        return self._tg.tasks

    # ---- queries -------------------------------------------------------------

    async def get_record(self, task_id: str) -> TaskRecord | None:
        async with self._sf() as db:
            return await q.fetch_record(db, self._team_run_id, task_id)

    async def get_all_tasks(self) -> list[TaskRecord]:
        async with self._sf() as db:
            return await q.fetch_all_records(db, self._team_run_id)

    async def get_adjacency(self) -> dict[str, list[str]]:
        async with self._sf() as db:
            return await q.fetch_adjacency(db, self._team_run_id)

    async def get_statuses(self) -> dict[str, str]:
        async with self._sf() as db:
            return await q.fetch_statuses(db, self._team_run_id)

    async def get_task_ids(self) -> set[str]:
        async with self._sf() as db:
            return await q.fetch_task_ids(db, self._team_run_id)

    async def get_done_sibling_ids(
        self,
        *,
        task_id: str,
        parent_id: str | None,
        since: float | None = None,
    ) -> list[str]:
        async with self._sf() as db:
            return await q.fetch_done_sibling_ids(
                db,
                self._team_run_id,
                task_id=task_id,
                parent_id=parent_id,
                since=since,
            )

    async def all_terminal(self) -> bool:
        async with self._sf() as db:
            return await q.count_non_terminal(db, self._team_run_id) == 0

    async def sibling_subtree_ids(self, parent_id: str | None) -> list[str]:
        async with self._sf() as db:
            return await q.fetch_sibling_subtree_ids(
                db, self._team_run_id, parent_id
            )

    async def get_siblings_and_descendants(
        self, initiating_task_id: str
    ) -> list[TaskRecord]:
        """Return all siblings of the initiating task plus their entire subtrees.

        Siblings share the same parent_id. Descendants are found via recursive
        CTE on parent_id. The initiating task itself is excluded.
        """
        async with self._sf() as db:
            return await q.fetch_siblings_and_descendants(
                db, self._team_run_id, initiating_task_id
            )

    # ---- mutations -----------------------------------------------------------

    async def mark_done(self, task_id: str) -> list[str]:
        async with self._sf() as db:
            await q.set_status_done(db, self._team_run_id, task_id)
            dependents = await q.fetch_pending_dependents_for_update(
                db, self._team_run_id, task_id
            )
            promoted_ids: list[str] = []
            for dep in dependents:
                unsatisfied = await q.fetch_unsatisfied_dep_ids(
                    db, self._team_run_id, list(dep.deps or [])
                )
                if not unsatisfied:
                    dep.status = "ready"
                    promoted_ids.append(dep.id)
            await db.commit()
        self._tg.mark_done(task_id, promoted_ids)
        return promoted_ids

    async def mark_expanded(self, task_id: str) -> None:
        async with self._sf() as db:
            await q.set_status_expanded(db, self._team_run_id, task_id)
            await db.commit()
        self._tg.mark_expanded(task_id)

    async def maybe_promote_expanded_parent(self, child_id: str) -> list[str]:
        promoted_all: list[str] = []
        current = child_id
        while True:
            async with self._sf() as db:
                row = await q.fetch_expanded_parent_candidate(
                    db, self._team_run_id, current
                )
            if row is None:
                break
            pid = str(row.id)
            if row.all_detached:
                await self.mark_terminal(pid, "failed", "all_children_detached")
            else:
                promoted = await self.mark_done(pid)
                promoted_all.extend(promoted)
            promoted_all.append(pid)
            current = pid
        return promoted_all

    async def mark_terminal(self, task_id: str, status: str, reason: str) -> None:
        async with self._sf() as db:
            await q.set_status_terminal(
                db, self._team_run_id, task_id, status, reason
            )
            await db.commit()
        self._tg.mark_terminal(task_id, status, reason)

    async def insert_plan(
        self,
        specs: list[TaskDefinition],
        parent_id: str | None = None,
        parent_depth: int = 0,
        parent_root_id: str | None = None,
    ) -> list[TaskRecord]:
        async with self._sf() as db:
            result_records = await q.insert_plan_records(
                db,
                self._team_run_id,
                specs,
                parent_id,
                parent_depth,
                parent_root_id,
            )
            await db.commit()
        self._tg.insert_tasks(record_to_task(rec) for rec in result_records)
        return result_records

    async def cascade_cancel_recursive(self, root_task_id: str) -> list[str]:
        async with self._sf() as db:
            cancelled = await q.cascade_cancel_recursive(
                db, self._team_run_id, root_task_id
            )
            await db.commit()
        self._tg.mark_cancelled(cancelled)
        return cancelled

    async def fail_orphaned_replanning(self) -> int:
        """Force-fail all REQUEST_REPLAN tasks whose replanner is terminal or missing.

        Origin tasks (A) are leaves — no subtree to cascade.
        """
        async with self._sf() as db:
            task_ids = await q.fetch_request_replan_ids(db, self._team_run_id)
            if not task_ids:
                return 0
            for task_id in task_ids:
                await q.set_status_terminal(
                    db,
                    self._team_run_id,
                    task_id,
                    "failed",
                    "orphaned_replanning_timeout",
                )
            await db.commit()
        await self.refresh_graph()
        return len(task_ids)

    async def finalize_replanned_origin(
        self, replanner_task_id: str
    ) -> str | None:
        """Mark the original REQUEST_REPLAN task terminal after its replanner succeeds."""
        async with self._sf() as db:
            origin_id = await q.fetch_replan_origin(
                db, self._team_run_id, replanner_task_id
            )
            if origin_id is None:
                return None
            rowcount = await q.finalize_replanned_origin(
                db, self._team_run_id, origin_id, replanner_task_id
            )
            await db.commit()
        if not rowcount:
            return None
        await self.refresh_graph()
        return origin_id

    async def fail_task(self, task_id: str, reason: str) -> None:
        """Mark a leaf task FAILED.

        Invariant: only leaf workers are allowed to fail. Non-leaf (EXPANDED)
        tasks only enter terminal states via promotion over their children.
        If an EXPANDED task somehow fails here, that is a team-run-level bug
        and callers should escalate via team_run.fail_fast.
        """
        async with self._sf() as db:
            status = await q.fetch_task_status(db, self._team_run_id, task_id)
            if status is None or status in ("done", "failed", "cancelled"):
                await db.commit()
                return
            if status == "expanded":
                raise GraphInvariantViolation(
                    f"fail_task: task {task_id} is EXPANDED; only leaf tasks may fail"
                )
            await q.set_status_failed_if_active(
                db, self._team_run_id, task_id, reason
            )
            await db.commit()
        await self.refresh_graph()

    async def cancel_all_pending(self) -> int:
        async with self._sf() as db:
            count = await q.cancel_statuses(
                db,
                self._team_run_id,
                ("pending", "ready", "expanded"),
                "team_run cancelled",
            )
            await db.commit()
            return count

    async def cancel_all_running(self, reason: str) -> int:
        async with self._sf() as db:
            count = await q.cancel_statuses(
                db, self._team_run_id, ("running",), reason
            )
            await db.commit()
            return count

    async def cancel_by_ids(self, task_ids: list[str], reason: str) -> int:
        if not task_ids:
            return 0
        async with self._sf() as db:
            count = await q.cancel_by_ids(
                db, self._team_run_id, task_ids, reason
            )
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
            cancelled_count = await q.cancel_by_ids(
                db, self._team_run_id, cancel_ids, cancel_reason
            )
            for cid in cancel_ids:
                await q.cascade_cancel_recursive(db, self._team_run_id, cid)
            inserted: list[TaskRecord] = []
            specs_by_parent: dict[str | None, list[TaskDefinition]] = defaultdict(list)
            for spec in specs:
                specs_by_parent[spec.parent_id].append(spec)
            for parent_id, grouped_specs in specs_by_parent.items():
                parent_depth = 0
                parent_root_id: str | None = None
                if parent_id is not None:
                    parent_depth, parent_root_id = (
                        await q.fetch_parent_depth_and_root(
                            db, self._team_run_id, parent_id
                        )
                    )
                inserted.extend(
                    await q.insert_plan_records(
                        db,
                        self._team_run_id,
                        grouped_specs,
                        parent_id,
                        parent_depth,
                        parent_root_id,
                    )
                )
            await db.commit()
        await self.refresh_graph()
        return cancelled_count, inserted

    async def mark_running(
        self, task_id: str, agent_run_id: str
    ) -> TaskRecord | None:
        async with self._sf() as db:
            rec = await q.mark_running(
                db, self._team_run_id, task_id, agent_run_id
            )
            if rec is not None:
                await q.assert_deps_satisfied(
                    db,
                    self._team_run_id,
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
            running = await q.fetch_running_records_for_update(
                db, self._team_run_id
            )
            for rec in running:
                await q.assert_deps_satisfied(
                    db,
                    self._team_run_id,
                    task_id=rec.id,
                    dep_ids=list(rec.deps or []),
                    transition="ready",
                )
            recs = await q.reset_running_to_ready(db, self._team_run_id)
            await db.commit()
            self._tg.recover_running(record_to_task(rec) for rec in recs)
            return recs

    async def replace_run_tasks(self, tasks: list[Task]) -> None:
        done_ids = {t.id for t in tasks if t.status == TaskStatus.DONE}
        for task in tasks:
            if task.status not in _STATUSES_REQUIRING_DONE_DEPS:
                continue
            unsatisfied = [
                dep_id for dep_id in task.deps if dep_id not in done_ids
            ]
            if unsatisfied:
                raise GraphInvariantViolation(
                    f"snapshot task {task.id!r} is {task.status.value} with "
                    f"unsatisfied dependencies: {', '.join(unsatisfied)}"
                )
        async with self._sf() as db:
            await q.delete_all_tasks(db, self._team_run_id)
            await q.insert_snapshot_tasks(db, self._team_run_id, tasks)
            await db.commit()
        self._tg.load(tasks)

    async def request_replan(
        self,
        task_id: str,
        reason: str,
        suggestion: str | None,
        replanner_agent: str,
    ) -> TaskRecord:
        async with self._sf() as db:
            rec = await q.fetch_replan_source(db, self._team_run_id, task_id)
            if rec is None:
                raise RuntimeError(f"replan: {task_id} not found")
            if rec.status in {s.value for s in TERMINAL_STATUSES}:
                raise GraphInvariantViolation(
                    f"request_replan: task {task_id} is terminal ({rec.status}); cannot replan"
                )
            # fired_by_task_id always points to the root original, not an
            # intermediate replanner, so recovery chains stay one-hop deep.
            root_origin = getattr(rec, "fired_by_task_id", None) or task_id
            # Idempotent per origin: if a live replanner already exists for this
            # failed origin, reuse it instead of spawning a parallel recovery branch.
            existing = await q.find_live_replanner_for_origin(
                db, self._team_run_id, root_origin
            )
            if existing is not None:
                return existing
            replanner_id = str(uuid.uuid4())
            if rec.status != "request_replan":
                await q.set_status_request_replan(
                    db, self._team_run_id, task_id, reason
                )
            task_text = f"Replan: {rec.agent_name} failed on task {task_id}: {reason}"
            if suggestion:
                task_text += f"\nSuggestion: {suggestion}"
            scope_paths = list(rec.scope_paths) if rec.scope_paths else []
            replanner = TaskRecord(
                id=replanner_id,
                team_run_id=self._team_run_id,
                agent_name=replanner_agent,
                objective=task_text,
                status="ready",
                deps=[],
                scope_paths=scope_paths,
                scope_ltree=[path_to_ltree(p) for p in scope_paths],
                parent_id=rec.parent_id,
                root_id=rec.root_id or "",
                depth=rec.depth or 0,
                fired_by_task_id=root_origin,
            )
            await q.insert_replanner_record(db, replanner)
            await q.replace_dependency(
                db,
                self._team_run_id,
                old_dep_id=task_id,
                new_dep_ids=[replanner_id],
            )
            await db.commit()
        await self.refresh_graph()
        return replanner
