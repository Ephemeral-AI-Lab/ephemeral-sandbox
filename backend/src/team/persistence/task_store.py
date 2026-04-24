"""TaskStore — SQL persistence layer for tasks.

Owns session lifecycle plus an in-memory task mirror. All SQLAlchemy
queries and the ``TaskRecord`` ORM live in :mod:`team.persistence.tasks_sql`.
"""

from __future__ import annotations

import uuid
from collections import defaultdict

from sqlalchemy.ext.asyncio import AsyncSession, async_sessionmaker

from team.core.errors import GraphInvariantViolation
from team.core.models import (
    TERMINAL_STATUSES,
    Task,
    TaskDefinition,
    TaskSpec,
    TaskStatus,
    _utcnow,
)
from team.persistence import tasks_sql as q
from team.persistence.ltree_utils import path_to_ltree
from team.persistence.tasks_sql import TaskRecord


def _has_replanner_role(agent_name: str) -> bool:
    from agents.registry import get_role

    return get_role(agent_name) == "replanner"


def record_to_task(rec: TaskRecord) -> Task:
    """Convert a TaskRecord ORM row to a domain Task."""
    return Task(
        id=rec.id,
        team_run_id=rec.team_run_id,
        definition=TaskDefinition(
            id=rec.id,
            spec=rec.spec,
            agent=rec.agent_name,
            description=rec.description or "",
            deps=list(rec.deps) if rec.deps else [],
            scope_paths=list(rec.scope_paths) if rec.scope_paths else [],
        ),
        status=TaskStatus.of(rec.status),
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
    raw queries to :mod:`tasks_sql` and mirrors the live graph in memory.
    """

    def __init__(
        self,
        session_factory: async_sessionmaker[AsyncSession],
        team_run_id: str,
    ) -> None:
        self._sf = session_factory
        self._team_run_id = team_run_id
        self._tasks: dict[str, Task] = {}

    # ---- in-memory graph proxy --------------------------------------------

    @property
    def graph(self) -> dict[str, Task]:
        return self._tasks

    @graph.setter
    def graph(self, value: dict[str, Task]) -> None:
        self._tasks = value

    def get_task(self, task_id: str) -> Task | None:
        """Fast in-memory lookup — no DB call."""
        return self._tasks.get(task_id)

    async def refresh_graph(self) -> dict[str, Task]:
        """Sync in-memory graph from DB. Returns the graph."""
        records = await self.get_all_tasks()
        tasks = [record_to_task(r) for r in records]
        self._tasks = {task.id: task for task in tasks}
        return self._tasks

    def _upsert(self, task: Task) -> None:
        self._tasks[task.id] = task

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

    async def all_terminal(self) -> bool:
        async with self._sf() as db:
            return await q.count_non_terminal(db, self._team_run_id) == 0

    # ---- mutations -----------------------------------------------------------

    async def mark_done(self, task_id: str) -> list[str]:
        async with self._sf() as db:
            await q.set_status(db, self._team_run_id, task_id, "done")
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
        task = self._tasks.get(task_id)
        if task is not None:
            task.status = TaskStatus.DONE
        for pid in promoted_ids:
            promoted = self._tasks.get(pid)
            if promoted is not None:
                promoted.status = TaskStatus.READY
        return promoted_ids

    async def mark_expanded(self, task_id: str) -> None:
        async with self._sf() as db:
            await q.set_status(db, self._team_run_id, task_id, "expanded")
            await db.commit()
        task = self._tasks.get(task_id)
        if task is not None:
            task.status = TaskStatus.EXPANDED

    async def fetch_promotable_parent(self, child_id: str) -> str | None:
        """Return the id of an EXPANDED parent of ``child_id`` ready to promote.

        "Ready to promote" means every live (non-detached) child has
        terminated. Detached statuses (failed/cancelled/request_replan) do
        not block promotion — the coordinator synthesizes the parent summary
        before calling :meth:`mark_done`.
        """
        async with self._sf() as db:
            return await q.fetch_expanded_parent_candidate(
                db, self._team_run_id, child_id
            )

    def terminal_child_ids(self) -> list[str]:
        """Return ids of every terminal child with a parent in the graph.

        Used after bulk graph changes (cascade-cancel, replan) so the coordinator
        can re-run promotion checks from each child upward.
        """
        return [
            task.id
            for task in self._tasks.values()
            if task.parent_id is not None and task.status in TERMINAL_STATUSES
        ]

    async def mark_terminal(self, task_id: str, status: str, reason: str) -> None:
        async with self._sf() as db:
            await q.set_status(db, self._team_run_id, task_id, status, reason)
            await db.commit()
        task = self._tasks.get(task_id)
        if task is not None:
            task.status = TaskStatus.of(status, default=TaskStatus.FAILED)
            task.failure_reason = reason

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
        for rec in result_records:
            task = record_to_task(rec)
            self._upsert(task)
        return result_records

    async def cascade_cancel_recursive(self, root_task_id: str) -> list[str]:
        async with self._sf() as db:
            cancelled = await q.cascade_cancel_recursive(
                db, self._team_run_id, root_task_id
            )
            await db.commit()
        for cid in cancelled:
            task = self._tasks.get(cid)
            if task is not None:
                task.status = TaskStatus.CANCELLED
        return cancelled

    async def finalize_replanned_origin(
        self, replanner_task_id: str
    ) -> str | None:
        """Mark the original REQUEST_REPLAN task terminal after its replanner succeeds."""
        async with self._sf() as db:
            replanner = await q.fetch_record(
                db, self._team_run_id, replanner_task_id
            )
            origin_id = replanner.fired_by_task_id if replanner else None
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

    async def mark_failed(self, task_id: str, reason: str) -> None:
        """Mark ``task_id`` FAILED regardless of its non-terminal status.

        Unified failure mutation for ``TaskCoordinator``: accepts
        RUNNING / EXPANDED / REQUEST_REPLAN /
        READY / PENDING. Already-terminal tasks are a no-op so repeated
        FAILED updates remain idempotent.
        """
        async with self._sf() as db:
            rec = await q.fetch_record(db, self._team_run_id, task_id)
        status = rec.status if rec else None
        if status is None or status in ("done", "failed", "cancelled"):
            return
        await self.mark_terminal(task_id, "failed", reason)

    async def cancel_all_pending(self) -> int:
        async with self._sf() as db:
            count = await q.bulk_cancel(
                db,
                self._team_run_id,
                statuses=("pending", "ready", "expanded"),
                reason="team_run cancelled",
            )
            await db.commit()
            return count

    async def cancel_all_running(self, reason: str) -> int:
        async with self._sf() as db:
            count = await q.bulk_cancel(
                db, self._team_run_id, statuses=("running",), reason=reason
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
            cancelled_count = await q.bulk_cancel(
                db,
                self._team_run_id,
                task_ids=cancel_ids,
                reason=cancel_reason,
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
                    parent = await q.fetch_record(db, self._team_run_id, parent_id)
                    if parent is None:
                        raise ValueError(f"replan parent '{parent_id}' not found")
                    parent_depth = parent.depth or 0
                    parent_root_id = parent.root_id or parent.id
                inserted.extend(
                    await q.insert_plan_records(
                        db,
                        self._team_run_id,
                        grouped_specs,
                        parent_id,
                        parent_depth,
                        parent_root_id,
                        child_depth=parent_depth if parent_id is not None else 0,
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
                unsatisfied = await q.fetch_unsatisfied_dep_ids(
                    db, self._team_run_id, list(rec.deps or [])
                )
                if unsatisfied:
                    raise GraphInvariantViolation(
                        f"task {rec.id!r} cannot transition to running; "
                        f"unsatisfied dependencies: {', '.join(unsatisfied)}"
                    )
            await db.commit()
        if rec is None:
            return None
        self._upsert(record_to_task(rec))
        return rec

    async def request_replan(
        self,
        task_id: str,
        reason: str,
        suggestion: str | None,
        replanner_agent: str,
    ) -> tuple[TaskRecord, bool]:
        async with self._sf() as db:
            rec = await q.fetch_record(db, self._team_run_id, task_id)
            if rec is None:
                raise RuntimeError(f"replan: {task_id} not found")
            if rec.status in {s.value for s in TERMINAL_STATUSES}:
                raise GraphInvariantViolation(
                    f"request_replan: task {task_id} is terminal ({rec.status}); cannot replan"
                )
            # fired_by_task_id always points to the root original, not an
            # intermediate replanner, so recovery chains stay one-hop deep.
            root_origin = rec.fired_by_task_id or task_id
            # Idempotent per origin: if a live replanner already exists for this
            # failed origin, reuse it instead of spawning a parallel recovery branch.
            # fired_by_task_id can also identify historical non-replanner trigger tasks,
            # so filter by role before reusing a live recovery task.
            candidates = await q.find_live_tasks_by_fired_origin(
                db, self._team_run_id, root_origin
            )
            existing_replanner = next(
                (
                    cand for cand in candidates
                    if _has_replanner_role(cand.agent_name)
                ),
                None,
            )
            if existing_replanner is not None:
                return existing_replanner, False
            replanner_id = str(uuid.uuid4())
            if rec.status != "request_replan":
                await q.set_status(
                    db, self._team_run_id, task_id, "request_replan", reason
                )
            task_text = f"Replan: {rec.agent_name} failed on task {task_id}: {reason}"
            if suggestion:
                task_text += f"\nSuggestion: {suggestion}"
            replan_spec = TaskSpec(
                goal=f"Replan failed task {task_id}.",
                detail=task_text,
                acceptance_criteria=(
                    "Submit exactly one corrective submit_replan payload with at "
                    "least one new task and explicit cancel_ids."
                ),
            )
            scope_paths = list(rec.scope_paths) if rec.scope_paths else []
            replanner = TaskRecord(
                id=replanner_id,
                team_run_id=self._team_run_id,
                agent_name=replanner_agent,
                spec=replan_spec.to_dict(),
                status="ready",
                deps=[],
                scope_paths=scope_paths,
                scope_ltree=[path_to_ltree(p) for p in scope_paths],
                parent_id=rec.parent_id,
                root_id=rec.root_id or "",
                depth=rec.depth or 0,
                fired_by_task_id=root_origin,
            )
            await q.insert_task_record(db, replanner)
            await q.replace_dependency(
                db,
                self._team_run_id,
                old_dep_id=task_id,
                new_dep_ids=[replanner_id],
            )
            await db.commit()
        await self.refresh_graph()
        return replanner, True
