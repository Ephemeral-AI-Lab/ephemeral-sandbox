"""Pure SQLAlchemy query functions for task persistence.

Each function takes an ``AsyncSession`` (caller owns the transaction) and a
``team_run_id`` scope. No session_factory, no in-memory graph, no commits —
those belong to :class:`team.persistence.task_store.TaskStore`.
"""

from __future__ import annotations

from collections import defaultdict, deque
from datetime import datetime, timezone

from sqlalchemy import Text, cast, delete, func, select, update
from sqlalchemy.dialects.postgresql import ARRAY
from sqlalchemy.engine import Row
from sqlalchemy.ext.asyncio import AsyncSession
from sqlalchemy.orm import aliased

from team.errors import GraphInvariantViolation
from team.models import TERMINAL_STATUSES, Task, TaskDefinition
from team.persistence.ltree_utils import path_to_ltree
from team.persistence.task_record import TaskRecord

# ---- reads --------------------------------------------------------------


async def fetch_record(
    db: AsyncSession, team_run_id: str, task_id: str
) -> TaskRecord | None:
    stmt = select(TaskRecord).where(
        TaskRecord.id == task_id,
        TaskRecord.team_run_id == team_run_id,
    )
    return (await db.execute(stmt)).scalar_one_or_none()


async def fetch_all_records(db: AsyncSession, team_run_id: str) -> list[TaskRecord]:
    stmt = (
        select(TaskRecord)
        .where(TaskRecord.team_run_id == team_run_id)
        .order_by(TaskRecord.depth, TaskRecord.created_at)
    )
    return list((await db.execute(stmt)).scalars().all())


async def fetch_adjacency(
    db: AsyncSession, team_run_id: str
) -> dict[str, list[str]]:
    stmt = select(TaskRecord.id, TaskRecord.deps).where(
        TaskRecord.team_run_id == team_run_id
    )
    rows = (await db.execute(stmt)).all()
    return {r.id: list(r.deps) if r.deps else [] for r in rows}


async def fetch_statuses(db: AsyncSession, team_run_id: str) -> dict[str, str]:
    stmt = select(TaskRecord.id, TaskRecord.status).where(
        TaskRecord.team_run_id == team_run_id
    )
    rows = (await db.execute(stmt)).all()
    return {r.id: r.status for r in rows}


async def fetch_task_ids(db: AsyncSession, team_run_id: str) -> set[str]:
    stmt = select(TaskRecord.id).where(TaskRecord.team_run_id == team_run_id)
    return {str(tid) for tid in (await db.execute(stmt)).scalars().all()}


async def fetch_done_sibling_ids(
    db: AsyncSession,
    team_run_id: str,
    *,
    task_id: str,
    parent_id: str | None,
    since: float | None = None,
) -> list[str]:
    stmt = (
        select(TaskRecord.id)
        .where(
            TaskRecord.team_run_id == team_run_id,
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


async def count_non_terminal(db: AsyncSession, team_run_id: str) -> int:
    stmt = select(func.count()).where(
        TaskRecord.team_run_id == team_run_id,
        TaskRecord.status.notin_(("done", "failed", "cancelled")),
    )
    return int((await db.execute(stmt)).scalar() or 0)


async def fetch_sibling_subtree_ids(
    db: AsyncSession, team_run_id: str, parent_id: str | None
) -> list[str]:
    subtree = (
        select(TaskRecord.id)
        .where(
            TaskRecord.team_run_id == team_run_id,
            TaskRecord.parent_id.is_not_distinct_from(parent_id),
        )
        .cte("subtree", recursive=True)
    )
    child = aliased(TaskRecord, name="child")
    subtree = subtree.union_all(
        select(child.id)
        .join(subtree, child.parent_id == subtree.c.id)
        .where(child.team_run_id == team_run_id)
    )
    rows = (await db.execute(select(subtree.c.id))).all()
    return [str(r.id) for r in rows]


async def fetch_siblings_and_descendants(
    db: AsyncSession, team_run_id: str, initiating_task_id: str
) -> list[TaskRecord]:
    initiator = aliased(TaskRecord, name="initiator")
    parent_of_initiator = (
        select(initiator.parent_id)
        .where(
            initiator.id == initiating_task_id,
            initiator.team_run_id == team_run_id,
        )
        .scalar_subquery()
    )
    subtree = (
        select(TaskRecord.id)
        .where(
            TaskRecord.team_run_id == team_run_id,
            TaskRecord.parent_id.is_not_distinct_from(parent_of_initiator),
            TaskRecord.id != initiating_task_id,
        )
        .cte("subtree", recursive=True)
    )
    child = aliased(TaskRecord, name="child")
    subtree = subtree.union_all(
        select(child.id)
        .join(subtree, child.parent_id == subtree.c.id)
        .where(child.team_run_id == team_run_id)
    )
    stmt = (
        select(TaskRecord)
        .where(
            TaskRecord.team_run_id == team_run_id,
            TaskRecord.id.in_(select(subtree.c.id)),
        )
        .order_by(TaskRecord.depth, TaskRecord.created_at)
    )
    return list((await db.execute(stmt)).scalars().all())


async def fetch_pending_dependents_for_update(
    db: AsyncSession, team_run_id: str, dep_id: str
) -> list[TaskRecord]:
    stmt = (
        select(TaskRecord)
        .where(
            TaskRecord.team_run_id == team_run_id,
            TaskRecord.status == "pending",
            TaskRecord.deps.any(dep_id),
        )
        .with_for_update()
    )
    return list((await db.execute(stmt)).scalars().all())


async def fetch_done_dep_ids(
    db: AsyncSession, team_run_id: str, dep_ids: set[str]
) -> set[str]:
    if not dep_ids:
        return set()
    stmt = select(TaskRecord.id).where(
        TaskRecord.team_run_id == team_run_id,
        TaskRecord.id.in_(dep_ids),
        TaskRecord.status == "done",
    )
    return {str(row) for row in (await db.execute(stmt)).scalars().all()}


async def fetch_unsatisfied_dep_ids(
    db: AsyncSession, team_run_id: str, dep_ids: list[str]
) -> list[str]:
    if not dep_ids:
        return []
    stmt = select(TaskRecord.id, TaskRecord.status).where(
        TaskRecord.team_run_id == team_run_id,
        TaskRecord.id.in_(set(dep_ids)),
    )
    rows = (await db.execute(stmt)).all()
    statuses = {str(row.id): str(row.status) for row in rows}
    return [dep_id for dep_id in dep_ids if statuses.get(dep_id) != "done"]


async def assert_deps_satisfied(
    db: AsyncSession,
    team_run_id: str,
    *,
    task_id: str,
    dep_ids: list[str],
    transition: str,
) -> None:
    unsatisfied = await fetch_unsatisfied_dep_ids(db, team_run_id, dep_ids)
    if unsatisfied:
        raise GraphInvariantViolation(
            f"task {task_id!r} cannot transition to {transition}; "
            f"unsatisfied dependencies: {', '.join(unsatisfied)}"
        )


async def fetch_expanded_parent_candidate(
    db: AsyncSession, team_run_id: str, current_id: str
) -> Row | None:
    """Return (id, all_detached) for the parent of ``current_id`` if that parent
    is ``expanded`` and every non-detached child is ``done``. Otherwise None.
    """
    child = aliased(TaskRecord, name="child")
    parent_id_sub = (
        select(child.parent_id)
        .where(child.id == current_id, child.team_run_id == team_run_id)
        .scalar_subquery()
    )
    sibling = aliased(TaskRecord, name="sibling")
    non_detached_unresolved = (
        select(1)
        .where(
            sibling.parent_id == TaskRecord.id,
            sibling.team_run_id == team_run_id,
            sibling.status.notin_(("done", "failed", "cancelled")),
        )
        .exists()
    )
    all_children_detached = ~(
        select(1)
        .where(
            sibling.parent_id == TaskRecord.id,
            sibling.team_run_id == team_run_id,
            sibling.status == "done",
        )
        .exists()
    )
    stmt = select(
        TaskRecord.id, all_children_detached.label("all_detached")
    ).where(
        TaskRecord.id == parent_id_sub,
        TaskRecord.team_run_id == team_run_id,
        TaskRecord.status == "expanded",
        ~non_detached_unresolved,
    )
    return (await db.execute(stmt)).first()


async def fetch_replan_origin(
    db: AsyncSession, team_run_id: str, replanner_task_id: str
) -> str | None:
    row = (
        await db.execute(
            select(TaskRecord.fired_by_task_id).where(
                TaskRecord.team_run_id == team_run_id,
                TaskRecord.id == replanner_task_id,
            )
        )
    ).first()
    if row is None or row.fired_by_task_id is None:
        return None
    return str(row.fired_by_task_id)


async def fetch_replan_source(
    db: AsyncSession, team_run_id: str, task_id: str
) -> Row | None:
    stmt = select(
        TaskRecord.id,
        TaskRecord.parent_id,
        TaskRecord.root_id,
        TaskRecord.depth,
        TaskRecord.agent_name,
        TaskRecord.scope_paths,
        TaskRecord.status,
        TaskRecord.fired_by_task_id,
    ).where(TaskRecord.id == task_id, TaskRecord.team_run_id == team_run_id)
    return (await db.execute(stmt)).first()


async def fetch_task_status(
    db: AsyncSession, team_run_id: str, task_id: str
) -> str | None:
    row = (
        await db.execute(
            select(TaskRecord.status).where(
                TaskRecord.id == task_id,
                TaskRecord.team_run_id == team_run_id,
            )
        )
    ).first()
    return None if row is None else str(row.status)


async def fetch_request_replan_ids(
    db: AsyncSession, team_run_id: str
) -> list[str]:
    rows = (
        await db.execute(
            select(TaskRecord.id).where(
                TaskRecord.team_run_id == team_run_id,
                TaskRecord.status == "request_replan",
            )
        )
    ).scalars().all()
    return [str(r) for r in rows]


async def fetch_running_records_for_update(
    db: AsyncSession, team_run_id: str
) -> list[TaskRecord]:
    stmt = (
        select(TaskRecord)
        .where(
            TaskRecord.team_run_id == team_run_id,
            TaskRecord.status == "running",
        )
        .with_for_update()
    )
    return list((await db.execute(stmt)).scalars().all())


async def fetch_parent_depth_and_root(
    db: AsyncSession, team_run_id: str, parent_id: str
) -> tuple[int, str | None]:
    rec = (
        await db.execute(
            select(TaskRecord).where(
                TaskRecord.team_run_id == team_run_id,
                TaskRecord.id == parent_id,
            )
        )
    ).scalar_one_or_none()
    if rec is None:
        raise ValueError(f"replan parent '{parent_id}' not found")
    return rec.depth or 0, rec.root_id or rec.id


# ---- mutations ----------------------------------------------------------


async def set_status_done(
    db: AsyncSession, team_run_id: str, task_id: str
) -> None:
    await db.execute(
        update(TaskRecord)
        .where(
            TaskRecord.id == task_id,
            TaskRecord.team_run_id == team_run_id,
        )
        .values(status="done", finished_at=func.now())
    )


async def set_status_expanded(
    db: AsyncSession, team_run_id: str, task_id: str
) -> None:
    await db.execute(
        update(TaskRecord)
        .where(
            TaskRecord.id == task_id,
            TaskRecord.team_run_id == team_run_id,
        )
        .values(status="expanded")
    )


async def set_status_terminal(
    db: AsyncSession, team_run_id: str, task_id: str, status: str, reason: str
) -> None:
    await db.execute(
        update(TaskRecord)
        .where(
            TaskRecord.id == task_id,
            TaskRecord.team_run_id == team_run_id,
        )
        .values(status=status, finished_at=func.now(), failure_reason=reason)
    )


async def set_status_failed_if_active(
    db: AsyncSession, team_run_id: str, task_id: str, reason: str
) -> None:
    await db.execute(
        update(TaskRecord)
        .where(TaskRecord.id == task_id, TaskRecord.team_run_id == team_run_id)
        .values(status="failed", finished_at=func.now(), failure_reason=reason)
    )


async def replace_dependency(
    db: AsyncSession,
    team_run_id: str,
    *,
    old_dep_id: str,
    new_dep_ids: list[str],
) -> list[str]:
    bad = (
        await db.execute(
            select(TaskRecord.id, TaskRecord.status).where(
                TaskRecord.team_run_id == team_run_id,
                TaskRecord.deps.any(old_dep_id),
                TaskRecord.status != "pending",
            )
        )
    ).all()
    if bad:
        details = ", ".join(f"{r.id}:{r.status}" for r in bad)
        raise GraphInvariantViolation(
            "replan dependency invariant violated: "
            f"tasks depending on {old_dep_id!r} must be pending; found {details}"
        )
    new_deps_expr = func.array_cat(
        func.array_remove(TaskRecord.deps, old_dep_id),
        cast(new_dep_ids, ARRAY(Text)),
    )
    result = await db.execute(
        update(TaskRecord)
        .where(
            TaskRecord.team_run_id == team_run_id,
            TaskRecord.deps.any(old_dep_id),
        )
        .values(
            deps=new_deps_expr,
            started_at=None,
            agent_run_id=None,
        )
        .returning(TaskRecord.id)
        .execution_options(synchronize_session=False)
    )
    return [str(r.id) for r in result.fetchall()]


async def insert_plan_records(
    db: AsyncSession,
    team_run_id: str,
    specs: list[TaskDefinition],
    parent_id: str | None,
    parent_depth: int,
    parent_root_id: str | None,
) -> list[TaskRecord]:
    if not specs:
        return []
    all_dep_ids = {dep_id for spec in specs for dep_id in spec.deps}
    done_ids = await fetch_done_dep_ids(db, team_run_id, all_dep_ids)
    records: list[TaskRecord] = []
    for spec in specs:
        status = "ready" if all(dep_id in done_ids for dep_id in spec.deps) else "pending"
        root_id = parent_root_id if parent_id else spec.id
        records.append(
            TaskRecord(
                id=spec.id,
                team_run_id=team_run_id,
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
            )
        )
    db.add_all(records)
    await db.flush()
    inserted_ids = [record.id for record in records]
    stmt = (
        select(TaskRecord)
        .where(
            TaskRecord.team_run_id == team_run_id,
            TaskRecord.id.in_(inserted_ids),
        )
        .order_by(TaskRecord.depth, TaskRecord.created_at)
    )
    return list((await db.execute(stmt)).scalars().all())


async def cascade_cancel_recursive(
    db: AsyncSession, team_run_id: str, root_task_id: str
) -> list[str]:
    active_rows = list(
        (
            await db.execute(
                select(TaskRecord).where(
                    TaskRecord.team_run_id == team_run_id,
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
            if records_by_id.get(dep_id) is None:
                continue
            if dep_id not in cancelled:
                cancelled.add(dep_id)
                queue.append(dep_id)

    if not cancelled:
        return []

    stmt = (
        update(TaskRecord)
        .where(
            TaskRecord.team_run_id == team_run_id,
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


async def cancel_statuses(
    db: AsyncSession,
    team_run_id: str,
    statuses: tuple[str, ...],
    reason: str,
) -> int:
    result = await db.execute(
        update(TaskRecord)
        .where(
            TaskRecord.team_run_id == team_run_id,
            TaskRecord.status.in_(statuses),
        )
        .values(status="cancelled", finished_at=func.now(), failure_reason=reason)
    )
    return result.rowcount or 0


async def cancel_by_ids(
    db: AsyncSession,
    team_run_id: str,
    task_ids: list[str],
    reason: str,
) -> int:
    if not task_ids:
        return 0
    result = await db.execute(
        update(TaskRecord)
        .where(
            TaskRecord.team_run_id == team_run_id,
            TaskRecord.id.in_(task_ids),
            TaskRecord.status.notin_([s.value for s in TERMINAL_STATUSES]),
        )
        .values(status="cancelled", finished_at=func.now(), failure_reason=reason)
    )
    return result.rowcount or 0


async def mark_running(
    db: AsyncSession,
    team_run_id: str,
    task_id: str,
    agent_run_id: str,
) -> TaskRecord | None:
    stmt = (
        update(TaskRecord)
        .where(
            TaskRecord.id == task_id,
            TaskRecord.team_run_id == team_run_id,
            TaskRecord.status == "running",
        )
        .values(
            agent_run_id=agent_run_id,
            started_at=func.coalesce(TaskRecord.started_at, func.now()),
        )
        .returning(TaskRecord)
        .execution_options(synchronize_session=False)
    )
    return (await db.execute(stmt)).scalar_one_or_none()


async def reset_running_to_ready(
    db: AsyncSession, team_run_id: str
) -> list[TaskRecord]:
    stmt = (
        update(TaskRecord)
        .where(
            TaskRecord.team_run_id == team_run_id,
            TaskRecord.status == "running",
        )
        .values(status="ready", started_at=None, agent_run_id=None)
        .returning(TaskRecord)
        .execution_options(synchronize_session=False)
    )
    return list((await db.execute(stmt)).scalars().all())


async def delete_all_tasks(db: AsyncSession, team_run_id: str) -> None:
    await db.execute(
        delete(TaskRecord).where(TaskRecord.team_run_id == team_run_id)
    )


async def insert_snapshot_tasks(
    db: AsyncSession, team_run_id: str, tasks: list[Task]
) -> None:
    db.add_all(
        [
            TaskRecord(
                id=t.id,
                team_run_id=team_run_id,
                agent_name=t.agent_name,
                status=t.status.value,
                objective=t.objective,
                deps=list(t.deps),
                scope_paths=list(t.scope_paths),
                scope_ltree=[path_to_ltree(p) for p in t.scope_paths],
                parent_id=t.parent_id,
                root_id=t.root_id or "",
                depth=t.depth,
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


async def set_status_request_replan(
    db: AsyncSession, team_run_id: str, task_id: str, reason: str
) -> None:
    await db.execute(
        update(TaskRecord)
        .where(TaskRecord.id == task_id, TaskRecord.team_run_id == team_run_id)
        .values(
            status="request_replan",
            failure_reason=f"replan_requested: {reason}",
        )
    )


async def finalize_replanned_origin(
    db: AsyncSession,
    team_run_id: str,
    origin_id: str,
    replanner_task_id: str,
) -> int:
    result = await db.execute(
        update(TaskRecord)
        .where(
            TaskRecord.team_run_id == team_run_id,
            TaskRecord.id == origin_id,
            TaskRecord.status == "request_replan",
        )
        .values(
            status="failed",
            finished_at=func.now(),
            failure_reason=f"replanned_by:{replanner_task_id}",
        )
    )
    return result.rowcount or 0


async def insert_replanner_record(
    db: AsyncSession, record: TaskRecord
) -> None:
    db.add(record)
    await db.flush()
