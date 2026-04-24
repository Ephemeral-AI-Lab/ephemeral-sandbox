"""TaskStore — persistence CRUD for tasks.

The store no longer owns any mutation *rules*; all policy lives in
:class:`team.runtime.task_graph.TaskGraph`. TaskStore exposes:

- ``load_graph``         — hydrate the in-memory Task dict at startup
- ``persist(mutation)``  — flush one ``GraphMutation`` in a single transaction
- ``mark_running``       — DB-atomic worker claim (the one lockless carve-out)
- ``cancel_all_pending`` / ``cancel_all_running`` — run-level shutdown helpers
- ``get_record`` / ``all_terminal`` — read helpers used by run lifecycle
"""

from __future__ import annotations

from sqlalchemy.ext.asyncio import AsyncSession, async_sessionmaker

from team.core.errors import GraphInvariantViolation
from team.core.models import Task, TaskStatus, _utcnow
from team.persistence import tasks_sql as q
from team.persistence.ltree_utils import path_to_ltree
from team.persistence.tasks_sql import TaskRecord
from team.runtime.task_graph import GraphMutation, TaskGraph


def record_to_task(rec: TaskRecord) -> Task:
    """Convert a TaskRecord ORM row to a domain Task."""
    return Task(
        id=rec.id,
        team_run_id=rec.team_run_id,
        spec=rec.spec,
        agent=rec.agent_name,
        deps=list(rec.deps) if rec.deps else [],
        scope_paths=list(rec.scope_paths) if rec.scope_paths else [],
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
    """Persistence CRUD for tasks. Mutation rules live in ``TaskGraph``."""

    def __init__(
        self,
        session_factory: async_sessionmaker[AsyncSession],
        team_run_id: str,
    ) -> None:
        self._sf = session_factory
        self._team_run_id = team_run_id
        self._task_graph = TaskGraph()

    # ---- in-memory graph proxy -----------------------------------------

    @property
    def task_graph(self) -> TaskGraph:
        """The in-memory graph owner used by TaskCoordinator / PlanExpander."""
        return self._task_graph

    @property
    def graph(self) -> dict[str, Task]:
        """Dict view of the in-memory tasks. Callers that mutate the graph
        should use ``task_graph`` directly."""
        return self._task_graph.tasks

    def get_task(self, task_id: str) -> Task | None:
        """Fast in-memory lookup — no DB call."""
        return self._task_graph.get(task_id)

    # ---- hydration + persistence --------------------------------------

    async def load_graph(self) -> list[Task]:
        """Read every task for this run as domain ``Task`` objects.

        Callers (``TaskCenter``) hand the result to ``TaskGraph.replace_all``
        to hydrate the in-memory graph at startup.
        """
        records = await self.get_all_tasks()
        return [record_to_task(r) for r in records]

    async def persist(self, mutation: GraphMutation) -> None:
        """Flush one ``GraphMutation`` to the database in a single transaction.

        The mutation carries pre-computed status changes, inserts, dep
        rewires, and failure-reason patches from ``TaskGraph``. This method
        performs only CRUD; every rule (dependent promotion, cascade,
        rewire-invariant) has already been enforced upstream.
        """
        if mutation.is_empty():
            return
        async with self._sf() as db:
            for change in mutation.status_changes:
                await q.set_status(
                    db,
                    self._team_run_id,
                    change.task_id,
                    change.new_status.value,
                    change.reason,
                )
            for insert in mutation.inserts:
                await q.insert_task_record(db, self._task_to_record(insert.task))
            for rewire in mutation.rewires:
                await q.replace_dependency(
                    db,
                    self._team_run_id,
                    old_dep_id=rewire.old_dep_id,
                    new_dep_ids=list(rewire.new_dep_ids),
                )
            for patch in mutation.failure_reason_patches:
                await q.set_failure_reason(
                    db,
                    self._team_run_id,
                    patch.task_id,
                    patch.failure_reason,
                )
            await db.commit()

    def _task_to_record(self, task: Task) -> TaskRecord:
        return TaskRecord(
            id=task.id,
            team_run_id=task.team_run_id,
            agent_name=task.agent,
            status=task.status.value,
            spec=task.spec.to_dict(),
            deps=list(task.deps),
            scope_paths=list(task.scope_paths),
            scope_ltree=[path_to_ltree(p) for p in task.scope_paths],
            parent_id=task.parent_id,
            root_id=task.root_id or "",
            depth=task.depth or 0,
            fired_by_task_id=task.fired_by_task_id,
        )

    # ---- read helpers --------------------------------------------------

    async def get_record(self, task_id: str) -> TaskRecord | None:
        async with self._sf() as db:
            return await q.fetch_record(db, self._team_run_id, task_id)

    async def get_all_tasks(self) -> list[TaskRecord]:
        async with self._sf() as db:
            return await q.fetch_all_records(db, self._team_run_id)

    async def all_terminal(self) -> bool:
        async with self._sf() as db:
            return await q.count_non_terminal(db, self._team_run_id) == 0

    # ---- run-level cancellation ----------------------------------------

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

    # ---- DB-atomic worker claim ----------------------------------------

    async def mark_running(
        self, task_id: str, agent_run_id: str
    ) -> TaskRecord | None:
        """Atomically claim a READY task for a worker. Lockless.

        This is the one carve-out from the in-memory-first pattern: multiple
        worker coroutines race to claim READY tasks, and serializing through
        the coordinator's lock would bottleneck the hot path. After a
        successful DB claim, the in-memory graph is updated to reflect the
        new RUNNING state.
        """
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
        self._task_graph.tasks[rec.id] = record_to_task(rec)
        return rec
