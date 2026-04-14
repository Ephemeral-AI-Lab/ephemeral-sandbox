"""SQLAlchemy ORM model for the ``tasks`` table (dispatcher work queue).

See Section 14.4 of the coordination redesign doc for schema.
This model is used by TaskCenter for durable task management.
The table is partitioned by team_run_id (LIST partitioning).
"""

from __future__ import annotations

from datetime import datetime, timezone
from typing import Any

from sqlalchemy import DateTime, Integer, String, Text
from sqlalchemy.dialects.postgresql import ARRAY
from sqlalchemy.orm import Mapped, mapped_column

from db.base import Base


def _utcnow() -> datetime:
    return datetime.now(timezone.utc)


TASK_RETURNING = (
    "id, team_run_id, agent_name, status, task,"
    " deps, scope_paths, scope_ltree,"
    " cascade_policy, parent_id, root_id, depth,"
    " pending_dep_count, retry_count, max_retries,"
    " agent_run_id, created_at, started_at,"
    " finished_at, failure_reason,"
    " blocker_id, pause_checkpoint, pause_verdict"
)


def row_to_record(row: Any) -> "TaskRecord":
    """Convert a raw SQL row to a TaskRecord instance."""
    return TaskRecord(
        id=row.id,
        team_run_id=row.team_run_id,
        agent_name=row.agent_name,
        status=row.status,
        task=row.task,
        deps=list(row.deps) if row.deps else [],
        scope_paths=list(row.scope_paths) if row.scope_paths else [],
        scope_ltree=list(row.scope_ltree) if getattr(row, "scope_ltree", None) else [],
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
        blocker_id=getattr(row, "blocker_id", None),
        pause_checkpoint=getattr(row, "pause_checkpoint", None),
        pause_verdict=getattr(row, "pause_verdict", None),
    )


class TaskRecord(Base):
    """Durable record of a task in the dispatcher work queue."""

    __tablename__ = "tasks"

    id: Mapped[str] = mapped_column(Text, primary_key=True)
    team_run_id: Mapped[str] = mapped_column(Text, primary_key=True)
    agent_name: Mapped[str] = mapped_column(Text, nullable=False)
    status: Mapped[str] = mapped_column(
        String(16), nullable=False, default="pending"
    )
    task: Mapped[str] = mapped_column(Text, nullable=False)
    deps: Mapped[list[str]] = mapped_column(
        ARRAY(Text), default=list
    )
    scope_paths: Mapped[list[str]] = mapped_column(
        ARRAY(Text), default=list
    )
    scope_ltree: Mapped[list[str]] = mapped_column(ARRAY(Text), default=list)
    cascade_policy: Mapped[str] = mapped_column(
        String(16), default="cancel"
    )
    parent_id: Mapped[str | None] = mapped_column(Text, nullable=True)
    root_id: Mapped[str] = mapped_column(Text, default="")
    depth: Mapped[int] = mapped_column(Integer, default=0)
    pending_dep_count: Mapped[int] = mapped_column(Integer, default=0)
    retry_count: Mapped[int] = mapped_column(Integer, default=0)
    max_retries: Mapped[int] = mapped_column(Integer, default=2)
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
    blocker_id: Mapped[str | None] = mapped_column(Text, nullable=True)
    pause_checkpoint: Mapped[str | None] = mapped_column(Text, nullable=True)
    pause_verdict: Mapped[str | None] = mapped_column(Text, nullable=True)

    def __repr__(self) -> str:
        return (
            f"<TaskRecord id={self.id!r} agent={self.agent_name!r} "
            f"status={self.status!r}>"
        )
