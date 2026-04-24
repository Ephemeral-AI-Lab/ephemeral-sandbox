"""Pure SQLAlchemy query functions for task persistence.

Each function takes an ``AsyncSession`` (caller owns the transaction) and a
``team_run_id`` scope. No session_factory, no in-memory graph, no commits —
those belong to :class:`team.persistence.task_store.TaskStore`.
"""

from __future__ import annotations

from collections import defaultdict, deque
from typing import Any, cast as type_cast

from sqlalchemy import Text, cast, func, select, update
from sqlalchemy.dialects.postgresql import ARRAY
from sqlalchemy.engine import CursorResult, Row
from sqlalchemy.ext.asyncio import AsyncSession
from sqlalchemy.orm import aliased

from team.core.errors import GraphInvariantViolation
from team.core.models import TERMINAL_STATUSES, TaskDefinition
from team.persistence.ltree_utils import path_to_ltree
from team.persistence.task_record import TaskRecord

# ---- reads --------------------------------------------------------------


def _rowcount(result: object) -> int:
    return int(type_cast(CursorResult[Any], result).rowcount or 0)


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


async def count_non_terminal(db: AsyncSession, team_run_id: str) -> int:
    stmt = select(func.count()).where(
        TaskRecord.team_run_id == team_run_id,
        TaskRecord.status.notin_([status.value for status in TERMINAL_STATUSES]),
    )
    return int((await db.execute(stmt)).scalar() or 0)


async def fetch_pending_dependents_for_update(
    db: AsyncSession, team_run_id: str, dep_id: str
) -> list[TaskRecord]:
    stmt = (
        select(TaskRecord)
        .where(
            TaskRecord.team_run_id == team_run_id,
            TaskRecord.status == "pending",
            TaskRecord.deps.contains([dep_id]),
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
) -> Row[Any] | None:
    """Return an expanded parent of ``current_id`` if live children are resolved.

    Failed, cancelled, and ``request_replan`` children are detached from
    promotion readiness. They do not synthesize parent failure; the caller
    resolves expandable parents through the normal summary/finalization path.
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
            sibling.status.notin_(
                ("done", "failed", "cancelled", "request_replan")
            ),
        )
        .exists()
    )
    stmt = select(TaskRecord.id).where(
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
) -> Row[Any] | None:
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


async def find_live_tasks_by_fired_origin(
    db: AsyncSession, team_run_id: str, origin_task_id: str
) -> list[TaskRecord]:
    """Return non-terminal tasks whose fired_by_task_id matches the origin.

    ``fired_by_task_id`` links both recovery replanners and parent-summary
    sidecars back to their trigger task, so callers must filter by role.
    """
    terminal = {s.value for s in TERMINAL_STATUSES}
    stmt = select(TaskRecord).where(
        TaskRecord.team_run_id == team_run_id,
        TaskRecord.fired_by_task_id == origin_task_id,
        TaskRecord.status.notin_(terminal),
    )
    return list((await db.execute(stmt)).scalars().all())


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


async def set_status_expanded_awaiting_summary(
    db: AsyncSession, team_run_id: str, task_id: str
) -> None:
    await db.execute(
        update(TaskRecord)
        .where(
            TaskRecord.id == task_id,
            TaskRecord.team_run_id == team_run_id,
        )
        .values(status="expanded_awaiting_summary")
    )


async def fetch_awaiting_summary_ids(
    db: AsyncSession, team_run_id: str
) -> list[str]:
    rows = (
        await db.execute(
            select(TaskRecord.id).where(
                TaskRecord.team_run_id == team_run_id,
                TaskRecord.status == "expanded_awaiting_summary",
            )
        )
    ).scalars().all()
    return [str(r) for r in rows]


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


async def replace_dependency(
    db: AsyncSession,
    team_run_id: str,
    *,
    old_dep_id: str,
    new_dep_ids: list[str],
) -> list[str]:
    violations = (
        await db.execute(
            select(TaskRecord.id, TaskRecord.status).where(
                TaskRecord.team_run_id == team_run_id,
                TaskRecord.deps.contains([old_dep_id]),
                TaskRecord.status != "pending",
            )
        )
    ).all()
    if violations:
        details = ", ".join(f"{r.id}:{r.status}" for r in violations)
        raise GraphInvariantViolation(
            "replan dependency invariant violated: "
            f"tasks depending on {old_dep_id!r} must be pending; found {details}"
        )
    updated_deps = func.array_cat(
        func.array_remove(TaskRecord.deps, old_dep_id),
        cast(new_dep_ids, ARRAY(Text)),
    )
    result = await db.execute(
        update(TaskRecord)
        .where(
            TaskRecord.team_run_id == team_run_id,
            TaskRecord.deps.contains([old_dep_id]),
        )
        .values(
            deps=updated_deps,
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
    *,
    child_depth: int | None = None,
) -> list[TaskRecord]:
    if not specs:
        return []
    all_dep_ids = {dep_id for spec in specs for dep_id in spec.deps}
    done_ids = await fetch_done_dep_ids(db, team_run_id, all_dep_ids)
    records: list[TaskRecord] = []
    record_depth = (
        child_depth
        if child_depth is not None
        else ((parent_depth + 1) if parent_id else 0)
    )
    for spec in specs:
        status = "ready" if all(dep_id in done_ids for dep_id in spec.deps) else "pending"
        root_id = parent_root_id if parent_id else spec.id
        records.append(
            TaskRecord(
                id=spec.id,
                team_run_id=team_run_id,
                agent_name=spec.agent,
                status=status,
                spec=spec.spec.to_dict(),
                description=spec.description or "",
                deps=list(spec.deps),
                scope_paths=list(spec.scope_paths),
                scope_ltree=[path_to_ltree(p) for p in spec.scope_paths],
                parent_id=parent_id,
                root_id=root_id or "",
                depth=record_depth,
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
    dependents_by_task_id: dict[str, list[str]] = defaultdict(list)
    for row in active_rows:
        if row.parent_id:
            children_by_parent[row.parent_id].append(row.id)
        for dep_id in row.deps or []:
            dependents_by_task_id[dep_id].append(row.id)

    cancelled: set[str] = set()
    queue: deque[str] = deque([root_task_id])
    while queue:
        current = queue.popleft()
        for child_id in children_by_parent.get(current, []):
            if child_id not in cancelled:
                cancelled.add(child_id)
                queue.append(child_id)
        for dependent_id in dependents_by_task_id.get(current, []):
            if records_by_id.get(dependent_id) is None:
                continue
            if dependent_id not in cancelled:
                cancelled.add(dependent_id)
                queue.append(dependent_id)

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
    return _rowcount(result)


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
    return _rowcount(result)


async def mark_running(
    db: AsyncSession,
    team_run_id: str,
    task_id: str,
    agent_run_id: str,
) -> TaskRecord | None:
    """Atomically claim a READY task (or re-claim an already-RUNNING one)."""
    stmt = (
        update(TaskRecord)
        .where(
            TaskRecord.id == task_id,
            TaskRecord.team_run_id == team_run_id,
            TaskRecord.status.in_(("ready", "running")),
        )
        .values(
            status="running",
            agent_run_id=agent_run_id,
            started_at=func.coalesce(TaskRecord.started_at, func.now()),
        )
        .returning(TaskRecord)
        .execution_options(synchronize_session=False)
    )
    return (await db.execute(stmt)).scalar_one_or_none()


async def set_status_request_replan(
    db: AsyncSession, team_run_id: str, task_id: str, reason: str
) -> None:
    await db.execute(
        update(TaskRecord)
        .where(TaskRecord.id == task_id, TaskRecord.team_run_id == team_run_id)
        .values(
            status="request_replan",
            finished_at=func.now(),
            failure_reason=f"replan_requested: {reason}",
        )
    )


async def finalize_replanned_origin(
    db: AsyncSession,
    team_run_id: str,
    origin_id: str,
    replanner_task_id: str,
) -> int:
    # REQUEST_REPLAN is terminal; origin A stays at REQUEST_REPLAN after the
    # replanner completes. Record the recovery linkage in failure_reason but
    # do not transition A out of its terminal state.
    result = await db.execute(
        update(TaskRecord)
        .where(
            TaskRecord.team_run_id == team_run_id,
            TaskRecord.id == origin_id,
            TaskRecord.status == "request_replan",
        )
        .values(
            failure_reason=f"replanned_by:{replanner_task_id}",
        )
    )
    return _rowcount(result)


async def insert_task_record(
    db: AsyncSession, record: TaskRecord
) -> None:
    db.add(record)
    await db.flush()
