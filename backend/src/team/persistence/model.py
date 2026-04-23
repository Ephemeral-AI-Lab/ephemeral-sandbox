"""Team definition persistence model."""

from __future__ import annotations

from datetime import datetime, timezone

from sqlalchemy import DateTime, String, Text
from sqlalchemy.dialects.postgresql import JSON
from sqlalchemy.orm import Mapped, mapped_column

from db.base import Base


def _utcnow() -> datetime:
    return datetime.now(timezone.utc)


class TeamDefinitionRecord(Base):
    """Role-based team composition stored in the database.

    ``planner_agent`` / ``worker_agents`` are the current durable columns.
    ``entry_planner`` / ``roster`` remain as compatibility mirrors so older
    code paths and stored events can still round-trip.
    Broken references are caught at ``TeamRun`` start time.
    """

    __tablename__ = "team_definitions"

    id: Mapped[str] = mapped_column(String(36), primary_key=True)
    name: Mapped[str] = mapped_column(String(128), unique=True, index=True)
    description: Mapped[str] = mapped_column(Text, default="")
    planner_agent: Mapped[str] = mapped_column(String(128))
    worker_agents: Mapped[list[str]] = mapped_column(JSON, default=list)
    roster: Mapped[dict[str, list[str]] | None] = mapped_column(JSON, default=dict, nullable=True)
    entry_planner: Mapped[str | None] = mapped_column(String(128), nullable=True)

    created_at: Mapped[datetime] = mapped_column(
        DateTime(timezone=True), default=_utcnow
    )
    updated_at: Mapped[datetime] = mapped_column(
        DateTime(timezone=True), default=_utcnow, onupdate=_utcnow
    )

    def __repr__(self) -> str:
        planner = self.entry_planner or self.planner_agent
        return f"<TeamDefinitionRecord name={self.name!r} planner={planner!r}>"
