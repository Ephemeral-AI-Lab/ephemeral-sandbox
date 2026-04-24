"""TaskStore — SQL persistence layer for tasks.

Owns session lifecycle + in-memory ``TaskGraph`` bookkeeping. All SQLAlchemy
queries live in :mod:`team.persistence.task_queries`.
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
from team.persistence import task_queries as q
from team.persistence.ltree_utils import path_to_ltree
from team.persistence.task_graph import TaskGraph
from team.persistence.task_record import TaskRecord


def _has_replanner_role(agent_name: str) -> bool:
    from agents.registry import get_role

    return get_role(agent_name) == "replanner"


def _has_parent_summarizer_role(agent_name: str) -> bool:
    from agents.registry import get_role

    return get_role(agent_name) == "parent_summarizer"


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

    async def all_terminal(self) -> bool:
        async with self._sf() as db:
            return await q.count_non_terminal(db, self._team_run_id) == 0

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

    async def mark_expanded_awaiting_summary(self, task_id: str) -> None:
        async with self._sf() as db:
            await q.set_status_expanded_awaiting_summary(
                db, self._team_run_id, task_id
            )
            await db.commit()
        self._tg.mark_expanded_awaiting_summary(task_id)

    @staticmethod
    def _is_expandable_parent_agent(agent_name: str) -> bool:
        """Return True if ``agent_name`` triggers the parent-summary handoff.

        Matches by agent *role* so a team roster can rename the canonical
        planners/replanners without silently disabling the summary handoff.
        Canonical agent names (``root_planner``, ``team_planner``,
        ``team_replanner``) are accepted as a defensive fallback when the
        registry is not yet populated (e.g. some unit tests).
        """
        if agent_name in {"root_planner", "team_planner", "team_replanner"}:
            return True
        try:
            from agents.registry import get_role

            return get_role(agent_name) in {"planner", "replanner"}
        except Exception:
            return False

    async def maybe_promote_expanded_parent(
        self, child_id: str
    ) -> tuple[list[str], list[str]]:
        """Resolve the chain of EXPANDED parents rooted at ``child_id``.

        Returns ``(promoted_ids, awaiting_summary_ids)``:

        - ``promoted_ids``: parents transitioned directly to DONE (non-expandable
          agent) or READY dependents promoted as a side effect of mark_done.
        - ``awaiting_summary_ids``: parents of planner/replanner role whose
          children all terminated; these transitioned to
          EXPANDED_AWAITING_SUMMARY and await a parent-summary sidecar.
        """
        promoted_all: list[str] = []
        awaiting_all: list[str] = []
        current = child_id
        while True:
            async with self._sf() as db:
                row = await q.fetch_expanded_parent_candidate(
                    db, self._team_run_id, current
                )
            if row is None:
                break
            pid = str(row.id)
            parent = self._tg.tasks.get(pid)
            parent_agent = parent.agent_name if parent is not None else ""
            if self._is_expandable_parent_agent(parent_agent):
                # Don't mark DONE yet — a parent-summary sidecar must run.
                # Detached children still need an authoritative roll-up; they
                # are not a synthetic parent failure.
                await self.mark_expanded_awaiting_summary(pid)
                awaiting_all.append(pid)
                # Stop the chain walk; grandparents must wait until this
                # parent actually transitions to DONE.
                break
            promoted = await self.mark_done(pid)
            promoted_all.extend(promoted)
            promoted_all.append(pid)
            current = pid
        return promoted_all, awaiting_all

    async def finalize_parent_summary(self, parent_id: str) -> list[str]:
        """Transition an awaiting-summary parent to DONE and promote dependents.

        Mirrors the tail of ``mark_done`` for the parent (sets DONE + promotes
        pending dependents whose deps are all satisfied). Returns newly-READY
        dependent ids.
        """
        return await self.mark_done(parent_id)

    async def insert_parent_summary_task(
        self,
        *,
        parent_task: Task,
        summarizer_agent: str,
        summary_prompt: str,
    ) -> tuple[Task, bool]:
        """Insert a READY parent-summary task as a child of the EAS parent.

        Uses ``fired_by_task_id = parent_task.id`` as the linkage and the
        ``parent_summarizer`` agent role as the discriminator. The summary task
        is a direct child of the awaiting-summary parent (depth = parent+1),
        which keeps it off the grandparent's promotion check because
        ``fetch_expanded_parent_candidate`` matches only ``status='expanded'``
        (not ``expanded_awaiting_summary``).

        Returns ``(task, created)``. Idempotent: if a live summary task already
        exists for the parent, the existing task is returned with
        ``created=False``.
        """
        async with self._sf() as db:
            candidates = await q.find_live_tasks_by_fired_origin(
                db, self._team_run_id, parent_task.id
            )
            existing = next(
                (
                    cand for cand in candidates
                    if _has_parent_summarizer_role(cand.agent_name)
                ),
                None,
            )
            if existing is not None:
                await db.commit()
                task = record_to_task(existing)
                self._tg.upsert(task)
                return task, False

            summary_id = str(uuid.uuid4())
            scope_paths = list(parent_task.definition.scope_paths or [])
            record = TaskRecord(
                id=summary_id,
                team_run_id=self._team_run_id,
                agent_name=summarizer_agent,
                spec=TaskSpec(
                    goal=f"Summarize parent task {parent_task.id}.",
                    detail=summary_prompt,
                    acceptance_criteria=(
                        "Submit a concise outcome summary for the parent task."
                    ),
                ).to_dict(),
                status="ready",
                deps=[],
                scope_paths=scope_paths,
                scope_ltree=[path_to_ltree(p) for p in scope_paths],
                parent_id=parent_task.id,
                root_id=parent_task.root_id or "",
                depth=(parent_task.depth or 0) + 1,
                fired_by_task_id=parent_task.id,
            )
            await q.insert_task_record(db, record)
            await db.commit()
        task = record_to_task(record)
        self._tg.upsert(task)
        return task, True

    async def fetch_parents_awaiting_summary(self) -> list[str]:
        """Return task ids currently stuck in expanded_awaiting_summary.

        Used on team-run restart to make sure parents the previous lifetime
        left mid-flight have a live parent-summary sidecar.
        """
        async with self._sf() as db:
            return await q.fetch_awaiting_summary_ids(db, self._team_run_id)

    async def sweep_expanded_promotions(
        self,
    ) -> tuple[list[str], list[str]]:
        """Resolve EXPANDED parents after bulk graph changes detach children.

        Returns ``(promoted_ids, awaiting_summary_ids)`` mirroring
        :meth:`maybe_promote_expanded_parent`.
        """
        promoted_all: list[str] = []
        awaiting_all: list[str] = []
        seen: set[str] = set()
        candidate_child_ids = [
            task.id
            for task in self._tg.tasks.values()
            if task.parent_id is not None and task.status in TERMINAL_STATUSES
        ]
        for child_id in candidate_child_ids:
            promoted, awaiting = await self.maybe_promote_expanded_parent(child_id)
            for promoted_id in promoted:
                if promoted_id in seen:
                    continue
                seen.add(promoted_id)
                promoted_all.append(promoted_id)
            for awaiting_id in awaiting:
                if awaiting_id in seen:
                    continue
                seen.add(awaiting_id)
                awaiting_all.append(awaiting_id)
        return promoted_all, awaiting_all

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

    async def mark_failed(self, task_id: str, reason: str) -> None:
        """Mark ``task_id`` FAILED regardless of its non-terminal status.

        Unified failure mutation for ``TaskStatusHandler``: accepts
        RUNNING / EXPANDED / EXPANDED_AWAITING_SUMMARY / REQUEST_REPLAN /
        READY / PENDING. Already-terminal tasks are a no-op so repeated
        FAILED updates remain idempotent.
        """
        async with self._sf() as db:
            status = await q.fetch_task_status(db, self._team_run_id, task_id)
        if status is None or status in ("done", "failed", "cancelled"):
            return
        await self.mark_terminal(task_id, "failed", reason)

    async def cancel_all_pending(self) -> int:
        async with self._sf() as db:
            count = await q.cancel_statuses(
                db,
                self._team_run_id,
                ("pending", "ready", "expanded", "expanded_awaiting_summary"),
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
        self._tg.remove_ready(task_id)
        return rec

    async def request_replan(
        self,
        task_id: str,
        reason: str,
        suggestion: str | None,
        replanner_agent: str,
    ) -> tuple[TaskRecord, bool]:
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
            # fired_by_task_id is shared with parent-summary tasks, so filter by role.
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
                await q.set_status_request_replan(
                    db, self._team_run_id, task_id, reason
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
