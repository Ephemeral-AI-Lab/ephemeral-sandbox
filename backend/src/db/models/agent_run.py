"""Agent run and response chunk models."""

from __future__ import annotations

from datetime import datetime, UTC

from sqlalchemy import DateTime, ForeignKey, Integer, String, Text
from sqlalchemy.dialects.postgresql import JSON
from sqlalchemy.orm import Mapped, mapped_column, relationship

from db.base import Base


class AgentRunRecord(Base):
    """A single agent execution within a session."""

    __tablename__ = "agent_runs"

    id: Mapped[str] = mapped_column(String(36), primary_key=True)
    session_id: Mapped[str] = mapped_column(
        String(36), ForeignKey("sessions.id", ondelete="CASCADE"), index=True
    )
    # When this run was spawned by another agent (e.g. via run_subagent),
    # parent_run_id points at the parent's agent_run id and parent_task_id
    # carries the parent's bg-task alias (e.g. "bg_3"). Top-level user-driven
    # runs leave both NULL. list_runs() filters parent_run_id IS NULL by
    # default so subagent runs do not pollute the user-facing transcript.
    parent_run_id: Mapped[str | None] = mapped_column(
        String(36),
        ForeignKey("agent_runs.id", ondelete="CASCADE"),
        nullable=True,
        index=True,
    )
    parent_task_id: Mapped[str | None] = mapped_column(String(64), nullable=True)
    agent_name: Mapped[str] = mapped_column(String(128))
    status: Mapped[str] = mapped_column(String(32), default="pending")
    input_query: Mapped[str | None] = mapped_column(Text, nullable=True)
    response: Mapped[dict | None] = mapped_column(JSON, nullable=True)
    message_history: Mapped[list | None] = mapped_column(JSON, nullable=True)
    compacted_history: Mapped[list | None] = mapped_column(JSON, nullable=True)
    reasoning: Mapped[str | None] = mapped_column(Text, nullable=True)
    error: Mapped[str | None] = mapped_column(Text, nullable=True)
    event_count: Mapped[int] = mapped_column(Integer, default=0)
    metadata_json: Mapped[dict | None] = mapped_column("metadata", JSON, nullable=True)
    started_at: Mapped[datetime | None] = mapped_column(DateTime(timezone=True), nullable=True)
    finished_at: Mapped[datetime | None] = mapped_column(DateTime(timezone=True), nullable=True)
    # Cancellation audit: populated when a run is terminated via
    # cancel_background_task. ``cancellation_reason`` mirrors the LLM-supplied
    # ``reason`` argument so the audit trail explains *why* the run was killed,
    # not just that it stopped.
    cancelled_at: Mapped[datetime | None] = mapped_column(DateTime(timezone=True), nullable=True)
    cancellation_reason: Mapped[str | None] = mapped_column(Text, nullable=True)
    created_at: Mapped[datetime] = mapped_column(
        DateTime(timezone=True), default=lambda: datetime.now(UTC)
    )

    # Relationships
    session: Mapped[SessionRecord] = relationship(back_populates="runs")  # noqa: F821

    def __repr__(self) -> str:
        return f"<AgentRunRecord id={self.id!r} agent={self.agent_name!r} status={self.status!r}>"
