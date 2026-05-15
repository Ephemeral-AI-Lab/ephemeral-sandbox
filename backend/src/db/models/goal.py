"""Goal persistence model — origin axis of harness work.

A Goal is created when a generator task calls
``submit_execution_handoff(goal)``. It owns an ordered list of
``Iteration`` ids representing the vertical (continuation) progression of
work toward the request's goal.
"""

from __future__ import annotations

from datetime import UTC, datetime

from sqlalchemy import JSON, DateTime, ForeignKey, String, Text
from sqlalchemy.orm import Mapped, mapped_column

from db.base import Base


class GoalRecord(Base):
    """Persisted Goal (origin axis)."""

    __tablename__ = "goals"

    id: Mapped[str] = mapped_column(String(36), primary_key=True)
    task_center_run_id: Mapped[str] = mapped_column(
        String(36),
        ForeignKey("task_center_runs.id", ondelete="CASCADE"),
        index=True,
    )
    requested_by_task_id: Mapped[str] = mapped_column(String(96), index=True)
    goal: Mapped[str] = mapped_column(Text)
    status: Mapped[str] = mapped_column(String(16))
    iteration_ids: Mapped[list[str]] = mapped_column(JSON, default=list)
    final_outcome: Mapped[dict | None] = mapped_column(JSON, nullable=True)
    created_at: Mapped[datetime] = mapped_column(
        DateTime(timezone=True), default=lambda: datetime.now(UTC)
    )
    updated_at: Mapped[datetime] = mapped_column(
        DateTime(timezone=True),
        default=lambda: datetime.now(UTC),
        onupdate=lambda: datetime.now(UTC),
    )
    closed_at: Mapped[datetime | None] = mapped_column(
        DateTime(timezone=True), nullable=True
    )
    def __repr__(self) -> str:
        return (
            f"<GoalRecord id={self.id!r} status={self.status!r}>"
        )
