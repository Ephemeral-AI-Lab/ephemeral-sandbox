"""PGDispatcher — PostgreSQL-backed work queue for team coordination.

Complete replacement for in-memory Dispatcher when PostgreSQL is available.
All state lives in PostgreSQL. No in-memory graph.

Uses ``FOR UPDATE SKIP LOCKED`` for atomic task claiming,
``pending_dep_count`` for dependency tracking, and recursive CTEs
for cascade operations.

See Section 14.6 of the coordination redesign doc.
"""

from __future__ import annotations

import logging
import uuid
from datetime import datetime, timezone
from typing import Any

from sqlalchemy import select, text
from sqlalchemy.ext.asyncio import AsyncSession, async_sessionmaker

from team.models import Task, TaskSpec, TaskStatus, _utcnow
from team.persistence.ltree_utils import path_to_ltree
from team.persistence.task_note_record import TaskNoteRecord
from team.persistence.task_record import TaskRecord

logger = logging.getLogger(__name__)


class PGDispatcher:
    """Dispatcher backed entirely by PostgreSQL. No in-memory state.

    All mutation methods are self-contained SQL operations.
    """

    def __init__(self, session_factory: async_sessionmaker[AsyncSession]) -> None:
        self._sf = session_factory

    # ---- core work queue -------------------------------------------------

    async def pop_ready(self, run_id: str) -> TaskRecord | None:
        """Atomically claim the next ready task via FOR UPDATE SKIP LOCKED."""
        async with self._sf() as db:
            row = (
                await db.execute(
                    text("""
                        UPDATE tasks SET status = 'running', started_at = NOW()
                        WHERE (id, team_run_id) = (
                            SELECT t.id, t.team_run_id FROM tasks t
                            WHERE t.team_run_id = :run_id
                              AND t.status = 'ready'
                              AND t.pending_dep_count = 0
                            ORDER BY t.depth, t.created_at
                            LIMIT 1
                            FOR UPDATE SKIP LOCKED
                        )
                        RETURNING id, team_run_id, agent_name, status, task,
                                  deps, scope_paths, scope_ltree,
                                  cascade_policy, parent_id, root_id, depth,
                                  pending_dep_count, retry_count, max_retries,
                                  agent_run_id, created_at, started_at,
                                  finished_at, failure_reason
                    """),
                    {"run_id": run_id},
                )
            ).fetchone()
            await db.commit()
            return _row_to_record(row) if row else None

    async def mark_running(
        self,
        run_id: str,
        task_id: str,
        agent_run_id: str,
    ) -> TaskRecord | None:
        """Persist the agent run identity for an already-claimed task."""
        async with self._sf() as db:
            row = (
                await db.execute(
                    text("""
                        UPDATE tasks
                        SET agent_run_id = :agent_run_id,
                            started_at = COALESCE(started_at, NOW())
                        WHERE id = :task_id
                          AND team_run_id = :run_id
                          AND status = 'running'
                        RETURNING id, team_run_id, agent_name, status, task,
                                  deps, scope_paths, scope_ltree,
                                  cascade_policy, parent_id, root_id, depth,
                                  pending_dep_count, retry_count, max_retries,
                                  agent_run_id, created_at, started_at,
                                  finished_at, failure_reason
                    """),
                    {
                        "run_id": run_id,
                        "task_id": task_id,
                        "agent_run_id": agent_run_id,
                    },
                )
            ).fetchone()
            await db.commit()
            return _row_to_record(row) if row else None

    async def mark_done(self, task_id: str, run_id: str) -> list[str]:
        """Mark task done, decrement dependents, promote those reaching zero.

        Returns IDs of promoted tasks.
        """
        async with self._sf() as db:
            await db.execute(
                text(
                    "UPDATE tasks SET status = 'done', finished_at = NOW() "
                    "WHERE id = :task_id AND team_run_id = :run_id"
                ),
                {"task_id": task_id, "run_id": run_id},
            )
            promoted = (
                await db.execute(
                    text("""
                        UPDATE tasks t
                        SET pending_dep_count = pending_dep_count - 1,
                            status = CASE
                                WHEN pending_dep_count - 1 = 0 THEN 'ready'
                                ELSE status
                            END
                        WHERE t.team_run_id = :run_id
                          AND t.status = 'pending'
                          AND :task_id = ANY(t.deps)
                          AND t.pending_dep_count > 0
                        RETURNING CASE
                            WHEN pending_dep_count = 0 THEN t.id
                            ELSE NULL
                        END AS promoted_id
                    """),
                    {"run_id": run_id, "task_id": task_id},
                )
            ).fetchall()
            await db.commit()
            return [r.promoted_id for r in promoted if r.promoted_id is not None]

    async def mark_failed(self, task_id: str, run_id: str, reason: str) -> None:
        """Mark a task as failed."""
        async with self._sf() as db:
            await db.execute(
                text(
                    "UPDATE tasks SET status = 'failed', finished_at = NOW(), "
                    "failure_reason = :reason "
                    "WHERE id = :task_id AND team_run_id = :run_id"
                ),
                {"task_id": task_id, "run_id": run_id, "reason": reason},
            )
            await db.commit()

    async def mark_cancelled(self, task_id: str, run_id: str, reason: str) -> None:
        """Mark a task as cancelled."""
        async with self._sf() as db:
            await db.execute(
                text(
                    "UPDATE tasks SET status = 'cancelled', finished_at = NOW(), "
                    "failure_reason = :reason "
                    "WHERE id = :task_id AND team_run_id = :run_id"
                ),
                {"task_id": task_id, "run_id": run_id, "reason": reason},
            )
            await db.commit()

    # ---- plan insertion --------------------------------------------------

    async def insert_plan(
        self,
        run_id: str,
        tasks: list[TaskSpec],
        parent_id: str | None = None,
        parent_depth: int = 0,
        parent_root_id: str | None = None,
    ) -> list[TaskRecord]:
        """Insert plan tasks atomically. Roots start 'ready', others 'pending'.

        Catch-up pass decrements pending_dep_count for already-done deps.
        """
        async with self._sf() as db:
            records: list[TaskRecord] = []
            for spec in tasks:
                status = "ready" if not spec.deps else "pending"
                root_id = parent_root_id if parent_id else spec.id
                records.append(
                    TaskRecord(
                        id=spec.id,
                        team_run_id=run_id,
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

            await db.execute(
                text("""
                WITH already_done AS (
                    SELECT id FROM tasks
                    WHERE team_run_id = :run_id AND status = 'done'
                )
                UPDATE tasks t
                SET pending_dep_count = pending_dep_count - (
                        SELECT COUNT(*) FROM already_done ad
                        WHERE ad.id = ANY(t.deps)),
                    status = CASE
                        WHEN pending_dep_count - (
                            SELECT COUNT(*) FROM already_done ad
                            WHERE ad.id = ANY(t.deps)) = 0 THEN 'ready'
                        ELSE status END
                WHERE t.team_run_id = :run_id
                  AND t.status = 'pending'
                  AND t.deps && (SELECT array_agg(id) FROM already_done)
            """),
                {"run_id": run_id},
            )
            await db.commit()
            return records

    # ---- cascade cancel (recursive) --------------------------------------

    async def cascade_cancel_recursive(self, run_id: str, root_task_id: str) -> list[str]:
        """Recursively cancel all pending/ready tasks that transitively
        depend on root_task_id. Excludes 'continue' cascade_policy dependents
        (those are handled separately in fail_task with warning injection).
        Returns IDs of cancelled tasks."""
        async with self._sf() as db:
            result = await db.execute(
                text("""
                    WITH RECURSIVE dep_chain AS (
                        SELECT id, cascade_policy FROM tasks
                        WHERE team_run_id = :run_id
                          AND :task_id = ANY(deps)
                          AND status IN ('pending', 'ready')
                          AND cascade_policy != 'continue'
                        UNION
                        SELECT t.id, t.cascade_policy FROM tasks t
                        JOIN dep_chain dc ON dc.id = ANY(t.deps)
                        WHERE t.team_run_id = :run_id
                          AND t.status IN ('pending', 'ready')
                          AND t.cascade_policy != 'continue'
                    )
                    UPDATE tasks SET status = 'cancelled', finished_at = NOW(),
                        failure_reason = 'cascaded from ' || :task_id
                    WHERE team_run_id = :run_id
                      AND id IN (SELECT id FROM dep_chain)
                    RETURNING id
                """),
                {"run_id": run_id, "task_id": root_task_id},
            )
            cancelled = [r.id for r in result.fetchall()]
            await db.commit()
            return cancelled

    # ---- fail with cascade policy ----------------------------------------

    async def fail_task(self, run_id: str, task_id: str, reason: str) -> None:
        """Fail a task, respecting cascade policies of dependents.

        Handles retry_first: if any non-terminal dependent has retry_first
        policy and the task has retries remaining, retry instead of failing.
        Otherwise, marks failed and cascades per dependent policies.
        """
        async with self._sf() as db:
            # Fetch the task
            rec = (
                await db.execute(
                    text(
                        "SELECT id, status, retry_count, max_retries FROM tasks "
                        "WHERE id = :id AND team_run_id = :run_id"
                    ),
                    {"id": task_id, "run_id": run_id},
                )
            ).fetchone()
            if rec is None or rec.status in ("done", "failed", "cancelled"):
                await db.commit()
                return

            # Check retry_first policy on dependents
            if rec.retry_count < rec.max_retries:
                has_retry_first = (
                    await db.execute(
                        text("""
                        SELECT EXISTS (
                            SELECT 1 FROM tasks
                            WHERE team_run_id = :run_id
                              AND :task_id = ANY(deps)
                              AND cascade_policy = 'retry_first'
                              AND status NOT IN ('done', 'failed', 'cancelled')
                        )
                    """),
                        {"run_id": run_id, "task_id": task_id},
                    )
                ).scalar()
                if has_retry_first:
                    # Retry instead of failing
                    await db.execute(
                        text("""
                            UPDATE tasks SET
                                status = 'ready',
                                retry_count = retry_count + 1,
                                agent_run_id = NULL,
                                started_at = NULL,
                                finished_at = NULL,
                                failure_reason = NULL
                            WHERE id = :task_id AND team_run_id = :run_id
                        """),
                        {"task_id": task_id, "run_id": run_id},
                    )
                    await db.commit()
                    return

            # Mark failed
            await db.execute(
                text(
                    "UPDATE tasks SET status = 'failed', finished_at = NOW(), "
                    "failure_reason = :reason "
                    "WHERE id = :task_id AND team_run_id = :run_id"
                ),
                {"task_id": task_id, "run_id": run_id, "reason": reason},
            )

            # Inject warning notes for 'continue' dependents (they aren't cancelled)
            continue_deps = (
                await db.execute(
                    text("""
                        SELECT id FROM tasks
                        WHERE team_run_id = :run_id
                          AND :task_id = ANY(deps)
                          AND cascade_policy = 'continue'
                          AND status NOT IN ('done', 'failed', 'cancelled')
                    """),
                    {"run_id": run_id, "task_id": task_id},
                )
            ).fetchall()
            for dep_rec in continue_deps:
                note = TaskNoteRecord(
                    id=uuid.uuid4(),
                    team_run_id=run_id,
                    task_id=dep_rec.id,
                    agent_name="system",
                    content=f"Warning: dependency {task_id} failed: {reason}. Proceed with caution.",
                )
                db.add(note)
            await db.commit()

        # Cascade cancel 'cancel' policy dependents (separate transaction for recursive CTE)
        # 'continue' dependents were already handled above with warning notes
        await self.cascade_cancel_recursive(run_id, task_id)

    # ---- retry -----------------------------------------------------------

    async def retry_task(self, run_id: str, task_id: str, max_retries: int) -> bool:
        """Reset a running task for retry. Returns False if retries exhausted."""
        async with self._sf() as db:
            rec = (
                await db.execute(
                    text("SELECT retry_count FROM tasks WHERE id = :id AND team_run_id = :run_id"),
                    {"id": task_id, "run_id": run_id},
                )
            ).fetchone()
            if rec is None:
                return False

            if rec.retry_count >= max_retries:
                # Exhausted — mark failed
                await db.execute(
                    text(
                        "UPDATE tasks SET status = 'failed', finished_at = NOW(), "
                        "failure_reason = 'retry_exhausted' "
                        "WHERE id = :task_id AND team_run_id = :run_id"
                    ),
                    {"task_id": task_id, "run_id": run_id},
                )
                await db.commit()
                await self.cascade_cancel_recursive(run_id, task_id)
                return False

            await db.execute(
                text("""
                    UPDATE tasks SET
                        status = 'ready',
                        retry_count = retry_count + 1,
                        agent_run_id = NULL,
                        started_at = NULL,
                        finished_at = NULL,
                        failure_reason = NULL
                    WHERE id = :task_id AND team_run_id = :run_id
                """),
                {"task_id": task_id, "run_id": run_id},
            )
            await db.commit()
            return True

    # ---- bulk cancel -----------------------------------------------------

    async def cancel_all_pending(self, run_id: str) -> int:
        """Cancel all pending/ready tasks. Returns count."""
        async with self._sf() as db:
            result = await db.execute(
                text("""
                    UPDATE tasks SET status = 'cancelled', finished_at = NOW(),
                        failure_reason = 'team_run cancelled'
                    WHERE team_run_id = :run_id
                      AND status IN ('pending', 'ready')
                """),
                {"run_id": run_id},
            )
            await db.commit()
            return result.rowcount

    async def cancel_all_running(self, run_id: str, reason: str) -> int:
        """Cancel all running tasks. Returns count."""
        async with self._sf() as db:
            result = await db.execute(
                text("""
                    UPDATE tasks SET status = 'cancelled', finished_at = NOW(),
                        failure_reason = :reason
                    WHERE team_run_id = :run_id AND status = 'running'
                """),
                {"run_id": run_id, "reason": reason},
            )
            await db.commit()
            return result.rowcount

    # ---- replan ----------------------------------------------------------

    async def request_replan(
        self,
        run_id: str,
        task_id: str,
        reason: str,
        suggestion: str | None,
        replanner_agent: str,
    ) -> TaskRecord:
        """Fail task, cancel siblings+dependents, insert replanner.

        Returns the replanner TaskRecord.
        """
        cancelled_sibling_ids: list[str] = []
        async with self._sf() as db:
            # Fetch the failing task
            rec = (
                await db.execute(
                    text("""SELECT id, parent_id, root_id, depth, agent_name,
                        scope_paths FROM tasks
                     WHERE id = :id AND team_run_id = :run_id"""),
                    {"id": task_id, "run_id": run_id},
                )
            ).fetchone()
            if rec is None:
                raise RuntimeError(f"replan: {task_id} not found")

            # 1. Mark failed
            await db.execute(
                text(
                    "UPDATE tasks SET status = 'failed', finished_at = NOW(), "
                    "failure_reason = :reason "
                    "WHERE id = :task_id AND team_run_id = :run_id"
                ),
                {"task_id": task_id, "run_id": run_id, "reason": f"replan_requested: {reason}"},
            )

            # 2. Cancel pending/ready siblings (same parent, not self)
            cancelled_siblings = await db.execute(
                text("""
                    UPDATE tasks SET status = 'cancelled', finished_at = NOW(),
                        failure_reason = 'cancelled_by_replan_from_' || :task_id
                    WHERE team_run_id = :run_id
                      AND parent_id IS NOT DISTINCT FROM :parent_id
                      AND id != :task_id
                      AND status IN ('pending', 'ready')
                    RETURNING id
                """),
                {"run_id": run_id, "task_id": task_id, "parent_id": rec.parent_id},
            )
            cancelled_sibling_ids = [row.id for row in cancelled_siblings.fetchall()]
            await db.commit()

        # 3. Cascade cancel dependents of failed + cancelled
        await self.cascade_cancel_recursive(run_id, task_id)
        for sibling_id in cancelled_sibling_ids:
            await self.cascade_cancel_recursive(run_id, sibling_id)

        async with self._sf() as db:
            # 4. Collect done sibling IDs for replanner deps
            done_siblings = (
                await db.execute(
                    text("""
                    SELECT id FROM tasks
                    WHERE team_run_id = :run_id
                      AND parent_id IS NOT DISTINCT FROM :parent_id
                      AND id != :task_id
                      AND status = 'done'
                """),
                    {"run_id": run_id, "task_id": task_id, "parent_id": rec.parent_id},
                )
            ).fetchall()
            dep_ids = [r.id for r in done_siblings]

            # 5. Insert replanner task
            replanner_id = str(uuid.uuid4())
            task_text = f"Replan: {rec.agent_name} failed on task {task_id}: {reason}" + (
                f"\nSuggestion: {suggestion}" if suggestion else ""
            )
            scope_paths = list(rec.scope_paths) if rec.scope_paths else []
            replanner = TaskRecord(
                id=replanner_id,
                team_run_id=run_id,
                agent_name=replanner_agent,
                task=task_text,
                status="ready" if not dep_ids else "pending",
                deps=dep_ids,
                scope_paths=scope_paths,
                scope_ltree=[path_to_ltree(p) for p in scope_paths],
                parent_id=rec.parent_id,
                root_id=rec.root_id or "",
                depth=rec.depth or 0,
                pending_dep_count=len(dep_ids),
            )
            db.add(replanner)
            await db.commit()
            return replanner

    async def cancel_by_ids(self, run_id: str, task_ids: list[str], reason: str) -> int:
        """Cancel specific tasks by ID. Returns count."""
        if not task_ids:
            return 0
        async with self._sf() as db:
            result = await db.execute(
                text("""
                    UPDATE tasks SET status = 'cancelled', finished_at = NOW(),
                        failure_reason = :reason
                    WHERE team_run_id = :run_id
                      AND id = ANY(:ids)
                      AND status IN ('pending', 'ready')
                """),
                {"run_id": run_id, "ids": task_ids, "reason": reason},
            )
            await db.commit()
            return result.rowcount

    # ---- queries ---------------------------------------------------------

    async def get_task(self, task_id: str, run_id: str) -> TaskRecord | None:
        """Fetch a single task by ID."""
        async with self._sf() as db:
            stmt = select(TaskRecord).where(
                TaskRecord.id == task_id,
                TaskRecord.team_run_id == run_id,
            )
            result = await db.execute(stmt)
            return result.scalar_one_or_none()

    async def get_tasks_by_status(
        self,
        run_id: str,
        status: str,
    ) -> list[TaskRecord]:
        """Fetch all tasks with a given status."""
        async with self._sf() as db:
            stmt = (
                select(TaskRecord)
                .where(TaskRecord.team_run_id == run_id, TaskRecord.status == status)
                .order_by(TaskRecord.depth, TaskRecord.created_at)
            )
            result = await db.execute(stmt)
            return list(result.scalars().all())

    async def get_all_tasks(self, run_id: str) -> list[TaskRecord]:
        """Fetch all tasks for a run. Used by checkpoint/validation."""
        async with self._sf() as db:
            stmt = (
                select(TaskRecord)
                .where(TaskRecord.team_run_id == run_id)
                .order_by(TaskRecord.depth, TaskRecord.created_at)
            )
            result = await db.execute(stmt)
            return list(result.scalars().all())

    async def get_adjacency(self, run_id: str) -> dict[str, list[str]]:
        """Lightweight: just {id: deps} for cycle detection. No full load."""
        async with self._sf() as db:
            result = await db.execute(
                text("SELECT id, deps FROM tasks WHERE team_run_id = :run_id"),
                {"run_id": run_id},
            )
            return {r.id: list(r.deps) if r.deps else [] for r in result.fetchall()}

    async def get_statuses(self, run_id: str) -> dict[str, str]:
        """Lightweight: {id: status} for final status computation."""
        async with self._sf() as db:
            result = await db.execute(
                text("SELECT id, status FROM tasks WHERE team_run_id = :run_id"),
                {"run_id": run_id},
            )
            return {r.id: r.status for r in result.fetchall()}

    async def get_task_ids(self, run_id: str) -> set[str]:
        """Return all task IDs for a run."""
        async with self._sf() as db:
            result = await db.execute(
                text("SELECT id FROM tasks WHERE team_run_id = :run_id"),
                {"run_id": run_id},
            )
            return {str(row.id) for row in result.fetchall()}

    async def get_done_sibling_ids(
        self,
        run_id: str,
        *,
        task_id: str,
        parent_id: str | None,
        since: float | None = None,
    ) -> list[str]:
        """Return sibling task IDs completed since the given time."""
        params: dict[str, Any] = {
            "run_id": run_id,
            "task_id": task_id,
            "parent_id": parent_id,
        }
        since_clause = ""
        if since is not None:
            params["since"] = datetime.fromtimestamp(since, tz=timezone.utc)
            since_clause = " AND finished_at >= :since"
        async with self._sf() as db:
            result = await db.execute(
                text(f"""
                    SELECT id
                    FROM tasks
                    WHERE team_run_id = :run_id
                      AND parent_id IS NOT DISTINCT FROM :parent_id
                      AND id != :task_id
                      AND status = 'done'
                      {since_clause}
                    ORDER BY finished_at, created_at
                """),
                params,
            )
            return [str(row.id) for row in result.fetchall()]

    async def all_terminal(self, run_id: str) -> bool:
        """Check if all tasks are in a terminal state."""
        async with self._sf() as db:
            result = await db.execute(
                text("""
                    SELECT COUNT(*) FROM tasks
                    WHERE team_run_id = :run_id
                      AND status NOT IN ('done', 'failed', 'cancelled')
                """),
                {"run_id": run_id},
            )
            return result.scalar() == 0

    async def task_count(self, run_id: str) -> int:
        """Total task count for budget tracking."""
        async with self._sf() as db:
            result = await db.execute(
                text("SELECT COUNT(*) FROM tasks WHERE team_run_id = :run_id"),
                {"run_id": run_id},
            )
            return result.scalar() or 0

    # ---- crash recovery --------------------------------------------------

    async def recover_running(self, run_id: str) -> list[TaskRecord]:
        """Reset stuck 'running' tasks to 'ready' after crash."""
        async with self._sf() as db:
            result = await db.execute(
                text("""
                    UPDATE tasks
                    SET status = 'ready', started_at = NULL, agent_run_id = NULL
                    WHERE team_run_id = :run_id AND status = 'running'
                    RETURNING id, team_run_id, agent_name, status, task,
                              deps, scope_paths, scope_ltree,
                              cascade_policy, parent_id, root_id, depth,
                              pending_dep_count, retry_count, max_retries,
                              agent_run_id, created_at, started_at,
                              finished_at, failure_reason
                """),
                {"run_id": run_id},
            )
            rows = result.fetchall()
            await db.commit()
            return [_row_to_record(r) for r in rows]


def _row_to_record(row: Any) -> TaskRecord:
    """Convert a raw SQL row to a TaskRecord ORM instance."""
    return TaskRecord(
        id=row.id,
        team_run_id=row.team_run_id,
        agent_name=row.agent_name,
        status=row.status,
        task=row.task,
        deps=list(row.deps) if row.deps else [],
        scope_paths=list(row.scope_paths) if row.scope_paths else [],
        scope_ltree=list(row.scope_ltree) if row.scope_ltree else [],
        cascade_policy=row.cascade_policy,
        parent_id=row.parent_id,
        root_id=row.root_id or "",
        depth=row.depth,
        pending_dep_count=row.pending_dep_count,
        retry_count=row.retry_count,
        max_retries=row.max_retries,
        agent_run_id=row.agent_run_id,
        created_at=row.created_at,
        started_at=row.started_at,
        finished_at=row.finished_at,
        failure_reason=row.failure_reason,
    )
