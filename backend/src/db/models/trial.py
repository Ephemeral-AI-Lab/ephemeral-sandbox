"""Trial persistence model — horizontal-retry axis of harness work.

A Trial is one full planner -> generator -> evaluator run inside an
Iteration.
"""

from __future__ import annotations

from datetime import UTC, datetime

from sqlalchemy import JSON, DateTime, ForeignKey, Integer, String, Text, UniqueConstraint
from sqlalchemy.orm import Mapped, mapped_column

from db.base import Base


class TrialRecord(Base):
    """Persisted Trial (horizontal retry axis)."""

    __tablename__ = "trials"

    id: Mapped[str] = mapped_column(String(36), primary_key=True)
    iteration_id: Mapped[str] = mapped_column(
        String(36),
        ForeignKey("iterations.id", ondelete="CASCADE"),
        index=True,
    )
    trial_sequence_no: Mapped[int] = mapped_column(Integer)
    stage: Mapped[str] = mapped_column(String(16))
    status: Mapped[str] = mapped_column(String(16))
    planner_task_id: Mapped[str | None] = mapped_column(String(96), nullable=True)
    task_specification: Mapped[str | None] = mapped_column(Text, nullable=True)
    evaluation_criteria: Mapped[list[str]] = mapped_column(JSON, default=list)
    generator_task_ids: Mapped[list[str]] = mapped_column(JSON, default=list)
    evaluator_task_id: Mapped[str | None] = mapped_column(String(96), nullable=True)
    continuation_goal: Mapped[str | None] = mapped_column(Text, nullable=True)
    fail_reason: Mapped[str | None] = mapped_column(String(48), nullable=True)
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
    __table_args__ = (
        UniqueConstraint(
            "iteration_id",
            "trial_sequence_no",
            name="uq_trial_iteration_sequence",
        ),
    )

    def __repr__(self) -> str:
        return (
            f"<TrialRecord id={self.id!r} "
            f"seq={self.trial_sequence_no} stage={self.stage!r}>"
        )
