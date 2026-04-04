"""Agent definition persistence model."""

from __future__ import annotations

from datetime import datetime, timezone

from sqlalchemy import Boolean, DateTime, Integer, String, Text
from sqlalchemy.dialects.postgresql import JSON
from sqlalchemy.orm import Mapped, mapped_column

from ephemeralos.db.base import Base


class AgentDefinitionRecord(Base):
    """A user-created agent definition stored in the database."""

    __tablename__ = "agent_definitions"

    id: Mapped[str] = mapped_column(String(36), primary_key=True)
    name: Mapped[str] = mapped_column(String(128), unique=True, index=True)
    description: Mapped[str] = mapped_column(Text)

    # Prompt & behavior
    system_prompt: Mapped[str | None] = mapped_column(Text, nullable=True)
    model: Mapped[str | None] = mapped_column(String(128), nullable=True)
    effort: Mapped[str | None] = mapped_column(String(16), nullable=True)
    max_turns: Mapped[int | None] = mapped_column(Integer, nullable=True)

    # Toolkits & skills (JSON arrays)
    toolkits: Mapped[list | None] = mapped_column(JSON, nullable=True)
    skills: Mapped[list] = mapped_column(JSON, default=list)

    # Hooks (JSON object)
    hooks: Mapped[dict | None] = mapped_column(JSON, nullable=True)

    # Lifecycle
    background: Mapped[bool] = mapped_column(Boolean, default=False)
    initial_prompt: Mapped[str | None] = mapped_column(Text, nullable=True)
    subagent_type: Mapped[str] = mapped_column(String(64), default="general-purpose")

    # Metadata & versioning
    version: Mapped[int] = mapped_column(Integer, default=1)
    is_active: Mapped[bool] = mapped_column(Boolean, default=True, index=True)
    created_by: Mapped[str | None] = mapped_column(String(128), nullable=True)
    tags: Mapped[list | None] = mapped_column(JSON, nullable=True)
    metadata_json: Mapped[dict | None] = mapped_column("metadata", JSON, nullable=True)

    # Timestamps
    created_at: Mapped[datetime] = mapped_column(
        DateTime(timezone=True), default=lambda: datetime.now(timezone.utc)
    )
    updated_at: Mapped[datetime] = mapped_column(
        DateTime(timezone=True),
        default=lambda: datetime.now(timezone.utc),
        onupdate=lambda: datetime.now(timezone.utc),
    )

    def __repr__(self) -> str:
        return f"<AgentDefinitionRecord name={self.name!r} v{self.version} active={self.is_active}>"
