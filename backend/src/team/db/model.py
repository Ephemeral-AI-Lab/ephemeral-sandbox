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
    """A team composition blob stored in the database.

    ``planner_agent`` and each entry in ``worker_agents`` are name
    references into ``agents.registry``. No cross-store foreign keys —
    broken references are caught at ``TeamRun`` start time.
    """

    __tablename__ = "team_definitions"

    id: Mapped[str] = mapped_column(String(36), primary_key=True)
    name: Mapped[str] = mapped_column(String(128), unique=True, index=True)
    description: Mapped[str] = mapped_column(Text, default="")
    planner_agent: Mapped[str] = mapped_column(String(128))
    worker_agents: Mapped[list[str]] = mapped_column(JSON, default=list)

    created_at: Mapped[datetime] = mapped_column(
        DateTime(timezone=True), default=_utcnow
    )
    updated_at: Mapped[datetime] = mapped_column(
        DateTime(timezone=True), default=_utcnow, onupdate=_utcnow
    )

    def __repr__(self) -> str:
        return f"<TeamDefinitionRecord name={self.name!r} planner={self.planner_agent!r}>"
