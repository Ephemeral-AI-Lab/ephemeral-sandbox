"""Task SQL layer — ORM model + query functions for the ``tasks`` table.

Queries take an ``AsyncSession`` (caller owns the transaction) and a
``team_run_id`` scope. No session_factory, no in-memory graph, no commits —
those belong to :class:`team.persistence.task_store.TaskStore`.
"""

from __future__ import annotations

from collections import defaultdict, deque
from datetime import datetime, timezone
from typing import Any, cast as type_cast

from sqlalchemy import (
    DateTime,
    Integer,
    JSON,
    Text,
    cast,
    func,
    select,
    update,
)
from sqlalchemy.dialects.postgresql import ARRAY
from sqlalchemy.engine import CursorResult, Row
from sqlalchemy.ext.asyncio import AsyncSession
from sqlalchemy.orm import Mapped, aliased, mapped_column

from db.base import Base
from team.core.errors import GraphInvariantViolation
from team.core.models import TERMINAL_STATUSES, TaskDefinition
from team.persistence.ltree_utils import path_to_ltree

# ---- ORM ----------------------------------------------------------------


def _utcnow() -> datetime:
    return datetime.now(timezone.utc)


class TaskRecord(Base):
    """Durable record of a team task. Partitioned by ``team_run_id``."""

    __tablename__ = "tasks"

    id: Mapped[str] = mapped_column(Text, primary_key=True)
    team_run_id: Mapped[str] = mapped_column(Text, primary_key=True)
    agent_name: Mapped[str] = mapped_column(Text, nullable=False)
    status: Mapped[str] = mapped_column(Text, nullable=False, default="pending")
    spec: Mapped[dict[str, Any]] = mapped_column(JSON, nullable=False)
    description: Mapped[str] = mapped_column(Text, default="")
    deps: Mapped[list[str]] = mapped_column(ARRAY(Text), default=list)
    scope_paths: Mapped[list[str]] = mapped_column(ARRAY(Text), default=list)
    scope_ltree: Mapped[list[str]] = mapped_column(ARRAY(Text), default=list)
    parent_id: Mapped[str | None] = mapped_column(Text, nullable=True)
    root_id: Mapped[str] = mapped_column(Text, default="")
    depth: Mapped[int] = mapped_column(Integer, default=0)
    agent_run_id: Mapped[str | None] = mapped_column(Text, nullable=True)
    created_at: Mapped[datetime] = mapped_column(
        DateTime(timezone=True), default=_utcnow
    )
    started_at: Mapped[datetime | None] = mapped_column(
        DateTime(timezone=True), nullable=True
    )
    finished_at: Mapped[datetime | None] = mapped_column(
        DateTime(timezone=True), nullable=True
    )
    failure_reason: Mapped[str | None] = mapped_column(Text, nullable=True)
    fired_by_task_id: Mapped[str | None] = mapped_column(Text, nullable=True)


# ---- helpers ------------------------------------------------------------


def _rowcount(result: object) -> int:
    return int(type_cast(CursorResult[Any], result).rowcount or 0)


async def _update_task(
    db: AsyncSession, team_run_id: str, task_id: str, **values: Any
) -> None:
    """Apply ``values`` to a single (task_id, team_run_id) row."""
    await db.execute(
        update(TaskRecord)
        .where(
            TaskRecord.id == task_id,
            TaskRecord.team_run_id == team_run_id,
        )
        .values(**values)
    )


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
) -> str | None:
    """Return an expanded parent of ``current_id`` if all children resolved.

    Failed, cancelled, and ``request_replan`` children are detached — they
    don't block promotion readiness. Callers synthesize the parent summary
    from children before marking the parent DONE.
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
    row = (await db.execute(stmt)).first()
    return None if row is None else str(row.id)


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

    ``fired_by_task_id`` identifies recovery replanners. Callers still filter by
    role to avoid reusing any historical non-replanner trigger task.
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
    row = (
        await db.execute(
            select(TaskRecord.depth, TaskRecord.root_id, TaskRecord.id).where(
                TaskRecord.team_run_id == team_run_id,
                TaskRecord.id == parent_id,
            )
        )
    ).first()
    if row is None:
        raise ValueError(f"replan parent '{parent_id}' not found")
    return row.depth or 0, row.root_id or row.id


# ---- mutations ----------------------------------------------------------


async def set_status_done(
    db: AsyncSession, team_run_id: str, task_id: str
) -> None:
    await _update_task(
        db, team_run_id, task_id, status="done", finished_at=func.now()
    )


async def set_status_expanded(
    db: AsyncSession, team_run_id: str, task_id: str
) -> None:
    await _update_task(db, team_run_id, task_id, status="expanded")


async def set_status_terminal(
    db: AsyncSession, team_run_id: str, task_id: str, status: str, reason: str
) -> None:
    await _update_task(
        db,
        team_run_id,
        task_id,
        status=status,
        finished_at=func.now(),
        failure_reason=reason,
    )


async def set_status_request_replan(
    db: AsyncSession, team_run_id: str, task_id: str, reason: str
) -> None:
    await _update_task(
        db,
        team_run_id,
        task_id,
        status="request_replan",
        finished_at=func.now(),
        failure_reason=f"replan_requested: {reason}",
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
    done_ids: set[str] = set()
    if all_dep_ids:
        done_stmt = select(TaskRecord.id).where(
            TaskRecord.team_run_id == team_run_id,
            TaskRecord.id.in_(all_dep_ids),
            TaskRecord.status == "done",
        )
        done_ids = {str(r) for r in (await db.execute(done_stmt)).scalars().all()}
    record_depth = (
        child_depth
        if child_depth is not None
        else ((parent_depth + 1) if parent_id else 0)
    )
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
        .values(failure_reason=f"replanned_by:{replanner_task_id}")
    )
    return _rowcount(result)


async def insert_task_record(db: AsyncSession, record: TaskRecord) -> None:
    db.add(record)
    await db.flush()
