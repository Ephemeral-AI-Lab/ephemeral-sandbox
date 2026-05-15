"""Iteration persistence model — vertical-continuation axis of harness work.

An Iteration owns an ordered list of ``Trial`` ids representing the
horizontal (retry) progression within a single segment's trial budget.
"""

from __future__ import annotations

from datetime import UTC, datetime

from sqlalchemy import JSON, DateTime, ForeignKey, Integer, String, Text, UniqueConstraint
from sqlalchemy.orm import Mapped, mapped_column

from db.base import Base


class IterationRecord(Base):
    """Persisted Iteration (vertical continuation axis)."""

    __tablename__ = "iterations"

    id: Mapped[str] = mapped_column(String(36), primary_key=True)
    goal_id: Mapped[str] = mapped_column(
        String(36),
        ForeignKey("goals.id", ondelete="CASCADE"),
        index=True,
    )
    sequence_no: Mapped[int] = mapped_column(Integer)
    creation_reason: Mapped[str] = mapped_column(String(32))
    goal: Mapped[str] = mapped_column(Text)
    trial_budget: Mapped[int] = mapped_column(Integer)
    status: Mapped[str] = mapped_column(String(16))
    trial_ids: Mapped[list[str]] = mapped_column(JSON, default=list)
    continuation_goal: Mapped[str | None] = mapped_column(Text, nullable=True)
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

    # Denormalized projections from this segment's *passing* harness graph at
    # close time. Both null while open and on failed close. Used by the
    # context engine's ``planner`` recipe for prior-segment context.
    task_specification: Mapped[str | None] = mapped_column(
        Text, nullable=True
    )
    task_summary: Mapped[str | None] = mapped_column(Text, nullable=True)
    __table_args__ = (
        UniqueConstraint(
            "goal_id",
            "sequence_no",
            name="uq_iteration_goal_sequence",
        ),
    )

    def __repr__(self) -> str:
        return (
            f"<IterationRecord id={self.id!r} "
            f"seq={self.sequence_no} status={self.status!r}>"
        )
