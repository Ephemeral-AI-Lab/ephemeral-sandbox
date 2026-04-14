"""TaskStore — SQL persistence layer for tasks.

Extracted from TaskCenter to separate persistence from orchestration.
All raw SQL lives here; TaskCenter delegates to this class.
"""

from __future__ import annotations

import uuid
from datetime import datetime, timezone
from typing import Any

from sqlalchemy import select, text
from sqlalchemy.ext.asyncio import AsyncSession, async_sessionmaker

from team.models import Task, TaskSpec, TaskStatus, _utcnow
from team.persistence.ltree_utils import path_to_ltree
from team.persistence.task_record import TASK_RETURNING, TaskRecord, row_to_record


def record_to_task(rec: Any) -> Task:
    """Convert a TaskRecord (or any duck-typed object) to a domain Task."""
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
        pause_checkpoint=getattr(rec, "pause_checkpoint", None),
        pause_verdict=getattr(rec, "pause_verdict", None),
    )


class TaskStore:
    """SQL persistence for tasks. Owns session_factory and team_run_id."""

    def __init__(
        self,
        session_factory: async_sessionmaker[AsyncSession],
        team_run_id: str,
    ) -> None:
        self._sf = session_factory
        self._team_run_id = team_run_id

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
            result = await db.execute(
                text("SELECT id, deps FROM tasks WHERE team_run_id = :run_id"),
                {"run_id": self._team_run_id},
            )
            return {r.id: list(r.deps) if r.deps else [] for r in result.fetchall()}

    async def get_statuses(self) -> dict[str, str]:
        async with self._sf() as db:
            result = await db.execute(
                text("SELECT id, status FROM tasks WHERE team_run_id = :run_id"),
                {"run_id": self._team_run_id},
            )
            return {r.id: r.status for r in result.fetchall()}

    async def get_task_ids(self) -> set[str]:
        async with self._sf() as db:
            result = await db.execute(
                text("SELECT id FROM tasks WHERE team_run_id = :run_id"),
                {"run_id": self._team_run_id},
            )
            return {str(row.id) for row in result.fetchall()}

    async def get_done_sibling_ids(
        self, *, task_id: str, parent_id: str | None, since: float | None = None,
    ) -> list[str]:
        params: dict[str, Any] = {
            "run_id": self._team_run_id, "task_id": task_id, "parent_id": parent_id,
        }
        since_clause = ""
        if since is not None:
            params["since"] = datetime.fromtimestamp(since, tz=timezone.utc)
            since_clause = " AND finished_at >= :since"
        async with self._sf() as db:
            result = await db.execute(
                text(f"""
                    SELECT id FROM tasks
                    WHERE team_run_id = :run_id
                      AND parent_id IS NOT DISTINCT FROM :parent_id
                      AND id != :task_id AND status = 'done'{since_clause}
                    ORDER BY finished_at, created_at
                """),
                params,
            )
            return [str(row.id) for row in result.fetchall()]

    async def all_terminal(self) -> bool:
        async with self._sf() as db:
            result = await db.execute(
                text(
                    "SELECT COUNT(*) FROM tasks WHERE team_run_id = :run_id"
                    " AND status NOT IN ('done','failed','cancelled')"
                ),
                {"run_id": self._team_run_id},
            )
            return result.scalar() == 0

    async def sibling_stats(self, parent_id: str | None) -> dict[str, int]:
        async with self._sf() as db:
            result = await db.execute(
                text("""
                    SELECT status, COUNT(*) AS cnt, SUM(retry_count) AS retries
                    FROM tasks
                    WHERE team_run_id = :run_id AND parent_id IS NOT DISTINCT FROM :parent_id
                    GROUP BY status
                """),
                {"run_id": self._team_run_id, "parent_id": parent_id},
            )
            stats: dict[str, int] = {
                "done": 0, "failed": 0, "cancelled": 0, "running": 0,
                "pending": 0, "ready": 0, "expanded": 0, "retry_total": 0,
            }
            for row in result.fetchall():
                stats[row.status] = row.cnt
                stats["retry_total"] += int(row.retries or 0)
            return stats

    async def sibling_subtree_ids(self, parent_id: str | None) -> list[str]:
        async with self._sf() as db:
            result = await db.execute(text("""
                WITH RECURSIVE subtree AS (
                    SELECT id
                    FROM tasks
                    WHERE team_run_id = :rid
                      AND parent_id IS NOT DISTINCT FROM :pid
                    UNION ALL
                    SELECT child.id
                    FROM tasks child
                    JOIN subtree s ON child.parent_id = s.id
                    WHERE child.team_run_id = :rid
                )
                SELECT id FROM subtree
            """), {"rid": self._team_run_id, "pid": parent_id})
            return [str(row.id) for row in result.fetchall()]

    async def get_siblings_and_descendants(self, initiating_task_id: str) -> list[TaskRecord]:
        """Return all siblings of the initiating task plus their entire subtrees.

        Siblings share the same parent_id. Descendants are found via recursive
        CTE on parent_id. The initiating task itself is excluded.
        """
        async with self._sf() as db:
            result = await db.execute(text("""
                WITH initiator AS (
                    SELECT parent_id FROM tasks
                    WHERE id = :tid AND team_run_id = :rid
                ),
                siblings AS (
                    SELECT t.id FROM tasks t, initiator i
                    WHERE t.team_run_id = :rid
                      AND t.parent_id IS NOT DISTINCT FROM i.parent_id
                      AND t.id != :tid
                ),
                subtree AS (
                    SELECT t.id FROM tasks t
                    WHERE t.team_run_id = :rid AND t.id IN (SELECT id FROM siblings)
                    UNION ALL
                    SELECT c.id FROM tasks c
                    INNER JOIN subtree s ON c.parent_id = s.id
                    WHERE c.team_run_id = :rid
                )
                SELECT t.* FROM tasks t
                WHERE t.team_run_id = :rid AND t.id IN (SELECT id FROM subtree)
                ORDER BY t.depth, t.created_at
            """), {"rid": self._team_run_id, "tid": initiating_task_id})
            return [row_to_record(row) for row in result.fetchall()]

    # ---- mutations -----------------------------------------------------------

    async def mark_done(self, task_id: str) -> list[str]:
        async with self._sf() as db:
            await db.execute(
                text("UPDATE tasks SET status='done', finished_at=NOW() WHERE id=:tid AND team_run_id=:rid"),
                {"tid": task_id, "rid": self._team_run_id},
            )
            promoted = (await db.execute(text("""
                UPDATE tasks t
                SET pending_dep_count = pending_dep_count - 1,
                    status = CASE WHEN pending_dep_count - 1 = 0 THEN 'ready' ELSE status END
                WHERE t.team_run_id = :rid AND t.status = 'pending'
                  AND :tid = ANY(t.deps) AND t.pending_dep_count > 0
                RETURNING CASE WHEN pending_dep_count = 0 THEN t.id ELSE NULL END AS promoted_id
            """), {"rid": self._team_run_id, "tid": task_id})).fetchall()
            await db.commit()
            return [r.promoted_id for r in promoted if r.promoted_id is not None]

    async def mark_expanded(self, task_id: str) -> None:
        async with self._sf() as db:
            await db.execute(
                text("UPDATE tasks SET status='expanded' WHERE id=:tid AND team_run_id=:rid"),
                {"tid": task_id, "rid": self._team_run_id},
            )
            await db.commit()

    async def maybe_promote_expanded_parent(self, child_id: str) -> list[str]:
        promoted_all: list[str] = []
        current = child_id
        while True:
            async with self._sf() as db:
                row = (await db.execute(text("""
                    WITH child AS (SELECT parent_id FROM tasks WHERE id=:cid AND team_run_id=:rid)
                    SELECT p.id FROM tasks p, child c
                    WHERE p.id = c.parent_id AND p.team_run_id=:rid AND p.status='expanded'
                      AND NOT EXISTS (
                          SELECT 1 FROM tasks s WHERE s.parent_id=p.id AND s.team_run_id=:rid
                            AND s.status NOT IN ('done','failed','cancelled')
                      )
                """), {"cid": current, "rid": self._team_run_id})).fetchone()
            if row is None:
                break
            pid = str(row.id)
            promoted = await self.mark_done(pid)
            promoted_all.append(pid)
            promoted_all.extend(promoted)
            current = pid
        return promoted_all

    async def mark_terminal(self, task_id: str, status: str, reason: str) -> None:
        async with self._sf() as db:
            await db.execute(
                text(
                    "UPDATE tasks SET status=:status, finished_at=NOW(), failure_reason=:reason"
                    " WHERE id=:tid AND team_run_id=:rid"
                ),
                {"status": status, "tid": task_id, "rid": self._team_run_id, "reason": reason},
            )
            await db.commit()

    async def insert_plan(
        self, specs: list[TaskSpec], parent_id: str | None = None,
        parent_depth: int = 0, parent_root_id: str | None = None,
    ) -> list[TaskRecord]:
        async with self._sf() as db:
            records: list[TaskRecord] = []
            for spec in specs:
                status = "ready" if not spec.deps else "pending"
                root_id = parent_root_id if parent_id else spec.id
                records.append(TaskRecord(
                    id=spec.id, team_run_id=self._team_run_id, agent_name=spec.agent,
                    status=status, task=spec.task, deps=list(spec.deps),
                    scope_paths=list(spec.scope_paths),
                    scope_ltree=[path_to_ltree(p) for p in spec.scope_paths],
                    parent_id=parent_id, root_id=root_id or "",
                    depth=(parent_depth + 1) if parent_id else 0,
                    pending_dep_count=len(spec.deps),
                ))
            db.add_all(records)
            await db.flush()
            await db.execute(text("""
                WITH already_done AS (SELECT id FROM tasks WHERE team_run_id=:rid AND status='done')
                UPDATE tasks t
                SET pending_dep_count = pending_dep_count - (
                        SELECT COUNT(*) FROM already_done ad WHERE ad.id = ANY(t.deps)),
                    status = CASE
                        WHEN pending_dep_count - (
                            SELECT COUNT(*) FROM already_done ad WHERE ad.id = ANY(t.deps)) = 0
                        THEN 'ready' ELSE status END
                WHERE t.team_run_id=:rid AND t.status='pending'
                  AND t.deps && (SELECT array_agg(id) FROM already_done)
            """), {"rid": self._team_run_id})
            inserted_ids = [record.id for record in records]
            rows = []
            if inserted_ids:
                rows = (
                    await db.execute(
                        text(
                            f"SELECT {TASK_RETURNING} FROM tasks "
                            "WHERE team_run_id=:rid AND id = ANY(:ids) "
                            "ORDER BY depth, created_at"
                        ),
                        {"rid": self._team_run_id, "ids": inserted_ids},
                    )
                ).fetchall()
            await db.commit()
            return [row_to_record(row) for row in rows]

    async def cascade_cancel_recursive(self, root_task_id: str) -> list[str]:
        async with self._sf() as db:
            result = await db.execute(text("""
                WITH RECURSIVE dep_chain AS (
                    SELECT id FROM tasks WHERE team_run_id=:rid AND id=:tid
                    UNION ALL
                    SELECT t.id FROM tasks t
                    JOIN dep_chain dc ON (
                        (dc.id = ANY(t.deps) AND t.cascade_policy != 'continue')
                        OR t.parent_id = dc.id
                    )
                    WHERE t.team_run_id=:rid AND t.status IN ('pending','ready','expanded')
                )
                UPDATE tasks SET status='cancelled', finished_at=NOW(),
                    failure_reason='cascaded from ' || :tid
                WHERE team_run_id=:rid AND id IN (SELECT DISTINCT id FROM dep_chain WHERE id != :tid)
                RETURNING id
            """), {"rid": self._team_run_id, "tid": root_task_id})
            cancelled = [r.id for r in result.fetchall()]
            await db.commit()
            return cancelled

    async def fail_task(self, task_id: str, reason: str) -> list[tuple[str, str]]:
        warnings: list[tuple[str, str]] = []
        rid = self._team_run_id
        async with self._sf() as db:
            rec = (await db.execute(text(
                "SELECT id, status, retry_count, max_retries FROM tasks WHERE id=:id AND team_run_id=:rid"
            ), {"id": task_id, "rid": rid})).fetchone()
            if rec is None or rec.status in ("done", "failed", "cancelled"):
                await db.commit()
                return warnings
            is_infra = reason.startswith(("worker_exception:", "runner_exception:"))
            if rec.retry_count < rec.max_retries:
                should_retry = is_infra
                if not should_retry:
                    should_retry = (await db.execute(text("""
                        SELECT EXISTS (SELECT 1 FROM tasks WHERE team_run_id=:rid
                          AND :tid = ANY(deps) AND cascade_policy='retry_first'
                          AND status NOT IN ('done','failed','cancelled'))
                    """), {"rid": rid, "tid": task_id})).scalar()
                if should_retry:
                    await db.execute(text("""
                        UPDATE tasks SET status='ready', retry_count=retry_count+1,
                            agent_run_id=NULL, started_at=NULL, finished_at=NULL, failure_reason=NULL
                        WHERE id=:tid AND team_run_id=:rid
                    """), {"tid": task_id, "rid": rid})
                    await db.commit()
                    return warnings
            await db.execute(text(
                "UPDATE tasks SET status='failed', finished_at=NOW(), failure_reason=:reason "
                "WHERE id=:tid AND team_run_id=:rid"
            ), {"tid": task_id, "rid": rid, "reason": reason})
            cont = (await db.execute(text("""
                SELECT id FROM tasks WHERE team_run_id=:rid AND :tid = ANY(deps)
                  AND cascade_policy='continue' AND status NOT IN ('done','failed','cancelled')
            """), {"rid": rid, "tid": task_id})).fetchall()
            for dep in cont:
                warnings.append((dep.id, f"Warning: dependency {task_id} failed: {reason}. Proceed with caution."))
            await db.commit()
        await self.cascade_cancel_recursive(task_id)
        return warnings

    async def retry_task(self, task_id: str, max_retries: int) -> bool:
        rid = self._team_run_id
        async with self._sf() as db:
            rec = (await db.execute(text(
                "SELECT retry_count FROM tasks WHERE id=:id AND team_run_id=:rid"
            ), {"id": task_id, "rid": rid})).fetchone()
            if rec is None:
                return False
            if rec.retry_count >= max_retries:
                await db.execute(text(
                    "UPDATE tasks SET status='failed', finished_at=NOW(), failure_reason='retry_exhausted' "
                    "WHERE id=:tid AND team_run_id=:rid"
                ), {"tid": task_id, "rid": rid})
                await db.commit()
                await self.cascade_cancel_recursive(task_id)
                return False
            await db.execute(text("""
                UPDATE tasks SET status='ready', retry_count=retry_count+1,
                    agent_run_id=NULL, started_at=NULL, finished_at=NULL, failure_reason=NULL
                WHERE id=:tid AND team_run_id=:rid
            """), {"tid": task_id, "rid": rid})
            await db.commit()
            return True

    async def cancel_all_pending(self) -> int:
        async with self._sf() as db:
            result = await db.execute(text("""
                UPDATE tasks SET status='cancelled', finished_at=NOW(), failure_reason='team_run cancelled'
                WHERE team_run_id=:rid AND status IN ('pending','ready','expanded')
            """), {"rid": self._team_run_id})
            await db.commit()
            return result.rowcount

    async def cancel_all_running(self, reason: str) -> int:
        async with self._sf() as db:
            result = await db.execute(text(
                "UPDATE tasks SET status='cancelled', finished_at=NOW(), failure_reason=:reason "
                "WHERE team_run_id=:rid AND status='running'"
            ), {"rid": self._team_run_id, "reason": reason})
            await db.commit()
            return result.rowcount

    async def pause_running_task(
        self, task_id: str, blocker_id: str, checkpoint: str, verdict: str,
    ) -> bool:
        """Transition a RUNNING task to PAUSED with blocker metadata."""
        async with self._sf() as db:
            result = await db.execute(text(
                "UPDATE tasks SET status='paused', blocker_id=:bid, "
                "pause_checkpoint=:cp, pause_verdict=:v "
                "WHERE id=:tid AND team_run_id=:rid AND status='running'"
            ), {"tid": task_id, "rid": self._team_run_id, "bid": blocker_id, "cp": checkpoint, "v": verdict})
            await db.commit()
            return result.rowcount > 0

    async def resume_paused_tasks(self, blocker_id: str) -> int:
        """Transition all PAUSED tasks for a blocker back to READY."""
        async with self._sf() as db:
            result = await db.execute(text(
                "UPDATE tasks SET status='ready', blocker_id=NULL, "
                "agent_run_id=NULL, started_at=NULL, finished_at=NULL, failure_reason=NULL "
                "WHERE team_run_id=:rid AND blocker_id=:bid AND status='paused'"
            ), {"rid": self._team_run_id, "bid": blocker_id})
            await db.commit()
            return result.rowcount

    async def cancel_paused_tasks(self, blocker_id: str) -> int:
        """Cancel all PAUSED tasks for a failed blocker."""
        async with self._sf() as db:
            result = await db.execute(text(
                "UPDATE tasks SET status='cancelled', finished_at=NOW(), "
                "failure_reason='blocker_failed', blocker_id=NULL "
                "WHERE team_run_id=:rid AND blocker_id=:bid AND status='paused'"
            ), {"rid": self._team_run_id, "bid": blocker_id})
            await db.commit()
            return result.rowcount

    async def cancel_by_ids(self, task_ids: list[str], reason: str) -> int:
        if not task_ids:
            return 0
        async with self._sf() as db:
            result = await db.execute(text("""
                UPDATE tasks SET status='cancelled', finished_at=NOW(), failure_reason=:reason
                WHERE team_run_id=:rid AND id = ANY(:ids) AND status IN ('pending','ready','expanded')
            """), {"rid": self._team_run_id, "ids": task_ids, "reason": reason})
            await db.commit()
            return result.rowcount

    async def mark_running_sql(self, task_id: str, agent_run_id: str) -> TaskRecord | None:
        """SQL-only part of mark_running. Returns TaskRecord or None."""
        async with self._sf() as db:
            row = (await db.execute(text(f"""
                UPDATE tasks SET agent_run_id=:arid, started_at=COALESCE(started_at, NOW())
                WHERE id=:tid AND team_run_id=:rid AND status='running'
                RETURNING {TASK_RETURNING}
            """), {"rid": self._team_run_id, "tid": task_id, "arid": agent_run_id})).fetchone()
            await db.commit()
        if row is None:
            return None
        return row_to_record(row)

    async def recover_running(self) -> list[TaskRecord]:
        async with self._sf() as db:
            result = await db.execute(text(f"""
                UPDATE tasks SET status='ready', started_at=NULL, agent_run_id=NULL
                WHERE team_run_id=:rid AND status='running'
                RETURNING {TASK_RETURNING}
            """), {"rid": self._team_run_id})
            rows = result.fetchall()
            await db.commit()
            return [row_to_record(r) for r in rows]

    async def replace_run_tasks(self, tasks: list[Task]) -> None:
        done_ids = {t.id for t in tasks if t.status == TaskStatus.DONE}
        async with self._sf() as db:
            await db.execute(text("DELETE FROM tasks WHERE team_run_id=:rid"), {"rid": self._team_run_id})
            db.add_all([
                TaskRecord(
                    id=t.id, team_run_id=self._team_run_id, agent_name=t.agent_name,
                    status=t.status.value, task=t.task, deps=list(t.deps),
                    scope_paths=list(t.scope_paths),
                    scope_ltree=[path_to_ltree(p) for p in t.scope_paths],
                    cascade_policy=t.cascade_policy, parent_id=t.parent_id,
                    root_id=t.root_id or "", depth=t.depth,
                    pending_dep_count=len([d for d in t.deps if d not in done_ids]),
                    retry_count=t.retry_count, max_retries=t.max_retries,
                    agent_run_id=t.agent_run_id, created_at=t.created_at,
                    started_at=t.started_at, finished_at=t.finished_at,
                    failure_reason=t.failure_reason,
                    blocker_id=t.blocker_id,
                    pause_checkpoint=t.pause_checkpoint,
                    pause_verdict=t.pause_verdict,
                )
                for t in tasks
            ])
            await db.commit()

    async def request_replan(
        self, task_id: str, reason: str, suggestion: str | None, replanner_agent: str,
    ) -> TaskRecord:
        rid = self._team_run_id
        async with self._sf() as db:
            rec = (await db.execute(text(
                "SELECT id, parent_id, root_id, depth, agent_name, scope_paths "
                "FROM tasks WHERE id=:id AND team_run_id=:rid"
            ), {"id": task_id, "rid": rid})).fetchone()
            if rec is None:
                raise RuntimeError(f"replan: {task_id} not found")
            await db.execute(text(
                "UPDATE tasks SET status='failed', finished_at=NOW(), "
                "failure_reason=:reason WHERE id=:tid AND team_run_id=:rid"
            ), {"tid": task_id, "rid": rid, "reason": f"replan_requested: {reason}"})
            await db.commit()
        async with self._sf() as db:
            done_sibs = (await db.execute(text("""
                SELECT id FROM tasks WHERE team_run_id=:rid
                  AND parent_id IS NOT DISTINCT FROM :pid AND id != :tid AND status='done'
            """), {"rid": rid, "tid": task_id, "pid": rec.parent_id})).fetchall()
            dep_ids = [r.id for r in done_sibs]
            replanner_id = str(uuid.uuid4())
            task_text = f"Replan: {rec.agent_name} failed on task {task_id}: {reason}"
            if suggestion:
                task_text += f"\nSuggestion: {suggestion}"
            scope_paths = list(rec.scope_paths) if rec.scope_paths else []
            replanner = TaskRecord(
                id=replanner_id, team_run_id=rid, agent_name=replanner_agent,
                task=task_text, status="ready" if not dep_ids else "pending",
                deps=dep_ids, scope_paths=scope_paths,
                scope_ltree=[path_to_ltree(p) for p in scope_paths],
                parent_id=rec.parent_id, root_id=rec.root_id or "",
                depth=rec.depth or 0, pending_dep_count=len(dep_ids),
            )
            db.add(replanner)
            await db.commit()
            return replanner
