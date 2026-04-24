"""Task SQL — ORM model + query functions for ``tasks``. Caller owns the session,
transaction, and in-memory graph (see :class:`team.persistence.task_store.TaskStore`)."""

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
from sqlalchemy.engine import CursorResult
from sqlalchemy.ext.asyncio import AsyncSession
from sqlalchemy.orm import Mapped, aliased, mapped_column

from db.base import Base
from team.core.errors import GraphInvariantViolation
from team.core.models import TERMINAL_STATUSES, TaskDefinition
from team.persistence.ltree_utils import path_to_ltree


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
    created_at: Mapped[datetime] = mapped_column(DateTime(timezone=True), default=_utcnow)
    started_at: Mapped[datetime | None] = mapped_column(DateTime(timezone=True), nullable=True)
    finished_at: Mapped[datetime | None] = mapped_column(DateTime(timezone=True), nullable=True)
    failure_reason: Mapped[str | None] = mapped_column(Text, nullable=True)
    fired_by_task_id: Mapped[str | None] = mapped_column(Text, nullable=True)


_TERMINAL = [s.value for s in TERMINAL_STATUSES]
_TERMINAL_ON_SET = {"done", "failed", "cancelled", "request_replan"}


async def fetch_record(db: AsyncSession, team_run_id: str, task_id: str) -> TaskRecord | None:
    return (await db.execute(
        select(TaskRecord).where(
            TaskRecord.id == task_id, TaskRecord.team_run_id == team_run_id
        )
    )).scalar_one_or_none()


async def fetch_all_records(db: AsyncSession, team_run_id: str) -> list[TaskRecord]:
    return list((await db.execute(
        select(TaskRecord)
        .where(TaskRecord.team_run_id == team_run_id)
        .order_by(TaskRecord.depth, TaskRecord.created_at)
    )).scalars().all())


async def fetch_adjacency(db: AsyncSession, team_run_id: str) -> dict[str, list[str]]:
    rows = (await db.execute(
        select(TaskRecord.id, TaskRecord.deps).where(TaskRecord.team_run_id == team_run_id)
    )).all()
    return {r.id: list(r.deps) if r.deps else [] for r in rows}


async def count_non_terminal(db: AsyncSession, team_run_id: str) -> int:
    return int((await db.execute(
        select(func.count()).where(
            TaskRecord.team_run_id == team_run_id,
            TaskRecord.status.notin_(_TERMINAL),
        )
    )).scalar() or 0)


async def fetch_pending_dependents_for_update(
    db: AsyncSession, team_run_id: str, dep_id: str
) -> list[TaskRecord]:
    return list((await db.execute(
        select(TaskRecord)
        .where(
            TaskRecord.team_run_id == team_run_id,
            TaskRecord.status == "pending",
            TaskRecord.deps.contains([dep_id]),
        )
        .with_for_update()
    )).scalars().all())


async def fetch_unsatisfied_dep_ids(
    db: AsyncSession, team_run_id: str, dep_ids: list[str]
) -> list[str]:
    if not dep_ids:
        return []
    rows = (await db.execute(
        select(TaskRecord.id, TaskRecord.status).where(
            TaskRecord.team_run_id == team_run_id, TaskRecord.id.in_(set(dep_ids))
        )
    )).all()
    statuses = {str(r.id): str(r.status) for r in rows}
    return [d for d in dep_ids if statuses.get(d) != "done"]


async def fetch_expanded_parent_candidate(
    db: AsyncSession, team_run_id: str, current_id: str
) -> str | None:
    """Expanded parent of ``current_id`` whose non-detached children all resolved.

    Failed/cancelled/request_replan children are detached and don't block promotion.
    """
    child = aliased(TaskRecord, name="child")
    parent_sub = (
        select(child.parent_id)
        .where(child.id == current_id, child.team_run_id == team_run_id)
        .scalar_subquery()
    )
    sibling = aliased(TaskRecord, name="sibling")
    unresolved = (
        select(1)
        .where(
            sibling.parent_id == TaskRecord.id,
            sibling.team_run_id == team_run_id,
            sibling.status.notin_(("done", "failed", "cancelled", "request_replan")),
        )
        .exists()
    )
    row = (await db.execute(
        select(TaskRecord.id).where(
            TaskRecord.id == parent_sub,
            TaskRecord.team_run_id == team_run_id,
            TaskRecord.status == "expanded",
            ~unresolved,
        )
    )).first()
    return None if row is None else str(row.id)


async def find_live_tasks_by_fired_origin(
    db: AsyncSession, team_run_id: str, origin_task_id: str
) -> list[TaskRecord]:
    """Non-terminal tasks with ``fired_by_task_id == origin_task_id`` (callers filter by role)."""
    return list((await db.execute(
        select(TaskRecord).where(
            TaskRecord.team_run_id == team_run_id,
            TaskRecord.fired_by_task_id == origin_task_id,
            TaskRecord.status.notin_(_TERMINAL),
        )
    )).scalars().all())


async def set_status(
    db: AsyncSession,
    team_run_id: str,
    task_id: str,
    status: str,
    reason: str | None = None,
) -> None:
    """Transition to ``status``; terminal statuses stamp ``finished_at``,
    ``reason`` populates ``failure_reason`` (prefixed for ``request_replan``)."""
    values: dict[str, Any] = {"status": status}
    if status in _TERMINAL_ON_SET:
        values["finished_at"] = func.now()
    if reason is not None:
        values["failure_reason"] = (
            f"replan_requested: {reason}" if status == "request_replan" else reason
        )
    await db.execute(
        update(TaskRecord)
        .where(TaskRecord.id == task_id, TaskRecord.team_run_id == team_run_id)
        .values(**values)
    )


async def replace_dependency(
    db: AsyncSession, team_run_id: str, *, old_dep_id: str, new_dep_ids: list[str]
) -> list[str]:
    violations = (await db.execute(
        select(TaskRecord.id, TaskRecord.status).where(
            TaskRecord.team_run_id == team_run_id,
            TaskRecord.deps.contains([old_dep_id]),
            TaskRecord.status != "pending",
        )
    )).all()
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
        .values(deps=updated_deps, started_at=None, agent_run_id=None)
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
    all_deps = {d for s in specs for d in s.deps}
    done_ids: set[str] = set()
    if all_deps:
        done_ids = {str(r) for r in (await db.execute(
            select(TaskRecord.id).where(
                TaskRecord.team_run_id == team_run_id,
                TaskRecord.id.in_(all_deps),
                TaskRecord.status == "done",
            )
        )).scalars().all()}
    depth = child_depth if child_depth is not None else ((parent_depth + 1) if parent_id else 0)
    records = [
        TaskRecord(
            id=spec.id,
            team_run_id=team_run_id,
            agent_name=spec.agent,
            status="ready" if all(d in done_ids for d in spec.deps) else "pending",
            spec=spec.spec.to_dict(),
            description=spec.description or "",
            deps=list(spec.deps),
            scope_paths=list(spec.scope_paths),
            scope_ltree=[path_to_ltree(p) for p in spec.scope_paths],
            parent_id=parent_id,
            root_id=(parent_root_id if parent_id else spec.id) or "",
            depth=depth,
        )
        for spec in specs
    ]
    db.add_all(records)
    await db.flush()
    return list((await db.execute(
        select(TaskRecord)
        .where(
            TaskRecord.team_run_id == team_run_id,
            TaskRecord.id.in_([r.id for r in records]),
        )
        .order_by(TaskRecord.depth, TaskRecord.created_at)
    )).scalars().all())


async def cascade_cancel_recursive(
    db: AsyncSession, team_run_id: str, root_task_id: str
) -> list[str]:
    rows = list((await db.execute(
        select(TaskRecord).where(
            TaskRecord.team_run_id == team_run_id, TaskRecord.status.notin_(_TERMINAL)
        )
    )).scalars().all())
    if not rows:
        return []
    live_ids = {r.id for r in rows}
    children: dict[str, list[str]] = defaultdict(list)
    dependents: dict[str, list[str]] = defaultdict(list)
    for r in rows:
        if r.parent_id:
            children[r.parent_id].append(r.id)
        for dep in r.deps or []:
            dependents[dep].append(r.id)
    cancelled: set[str] = set()
    queue: deque[str] = deque([root_task_id])
    while queue:
        current = queue.popleft()
        for cid in children.get(current, []) + dependents.get(current, []):
            if cid in live_ids and cid not in cancelled:
                cancelled.add(cid)
                queue.append(cid)
    if not cancelled:
        return []
    result = await db.execute(
        update(TaskRecord)
        .where(TaskRecord.team_run_id == team_run_id, TaskRecord.id.in_(cancelled))
        .values(
            status="cancelled",
            finished_at=func.now(),
            failure_reason=f"cascaded from {root_task_id}",
        )
        .returning(TaskRecord.id)
        .execution_options(synchronize_session=False)
    )
    return [r.id for r in result.fetchall()]


async def bulk_cancel(
    db: AsyncSession,
    team_run_id: str,
    *,
    statuses: tuple[str, ...] | None = None,
    task_ids: list[str] | None = None,
    reason: str,
) -> int:
    """Cancel by current ``statuses`` or by ``task_ids`` (non-terminal only). Returns rowcount."""
    conditions = [TaskRecord.team_run_id == team_run_id]
    if statuses is not None:
        conditions.append(TaskRecord.status.in_(statuses))
    if task_ids is not None:
        if not task_ids:
            return 0
        conditions.extend([TaskRecord.id.in_(task_ids), TaskRecord.status.notin_(_TERMINAL)])
    result = await db.execute(
        update(TaskRecord).where(*conditions).values(
            status="cancelled", finished_at=func.now(), failure_reason=reason
        )
    )
    return int(type_cast(CursorResult[Any], result).rowcount or 0)


async def mark_running(
    db: AsyncSession, team_run_id: str, task_id: str, agent_run_id: str
) -> TaskRecord | None:
    """Atomically claim a READY task (or re-claim an already-RUNNING one)."""
    return (await db.execute(
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
    )).scalar_one_or_none()


async def finalize_replanned_origin(
    db: AsyncSession, team_run_id: str, origin_id: str, replanner_task_id: str
) -> int:
    # REQUEST_REPLAN is terminal; origin stays at REQUEST_REPLAN. Record only
    # the recovery linkage so failure_reason carries the replanner pointer.
    result = await db.execute(
        update(TaskRecord)
        .where(
            TaskRecord.team_run_id == team_run_id,
            TaskRecord.id == origin_id,
            TaskRecord.status == "request_replan",
        )
        .values(failure_reason=f"replanned_by:{replanner_task_id}")
    )
    return int(type_cast(CursorResult[Any], result).rowcount or 0)


async def insert_task_record(db: AsyncSession, record: TaskRecord) -> None:
    db.add(record)
    await db.flush()
