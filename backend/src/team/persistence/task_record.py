"""SQLAlchemy ORM model for the ``tasks`` table (dispatcher work queue).

See Section 14.4 of the coordination redesign doc for schema.
This model is used by TaskCenter for durable task management.
The table is partitioned by team_run_id (LIST partitioning).
"""

from __future__ import annotations

from datetime import datetime, timezone

from sqlalchemy import DateTime, Integer, String, Text
from sqlalchemy.dialects.postgresql import ARRAY
from sqlalchemy.orm import Mapped, mapped_column

from db.base import Base


def _utcnow() -> datetime:
    return datetime.now(timezone.utc)


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
    fired_by_task_id: Mapped[str | None] = mapped_column(Text, nullable=True)
    pause_checkpoint: Mapped[str | None] = mapped_column(Text, nullable=True)
    pause_verdict: Mapped[str | None] = mapped_column(Text, nullable=True)

    def __repr__(self) -> str:
        return (
            f"<TaskRecord id={self.id!r} agent={self.agent_name!r} "
            f"status={self.status!r}>"
        )
