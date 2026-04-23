"""Agent definition persistence model."""

from __future__ import annotations

from datetime import datetime, UTC
from typing import Any

from sqlalchemy import Boolean, DateTime, Integer, String, Text
from sqlalchemy.dialects.postgresql import JSON
from sqlalchemy.orm import Mapped, mapped_column

from db.base import Base


class AgentDefinitionRecord(Base):
    """A user-created agent definition stored in the database."""

    __tablename__ = "agent_definitions"

    id: Mapped[str] = mapped_column(String(36), primary_key=True)
    name: Mapped[str] = mapped_column(String(128), unique=True, index=True)
    description: Mapped[str] = mapped_column(Text)

    # Prompt & behavior
    system_prompt: Mapped[str | None] = mapped_column(Text, nullable=True)
    model: Mapped[str] = mapped_column(String(128), nullable=False)
    effort: Mapped[str | None] = mapped_column(String(16), nullable=True)
    tool_call_limit: Mapped[int | None] = mapped_column(Integer, nullable=True)

    # Toolkits, skills & tool restrictions (JSON arrays)
    toolkits: Mapped[list[str] | None] = mapped_column(JSON, nullable=True)
    skills: Mapped[list[str]] = mapped_column(JSON, default=list)
    allowed_tools: Mapped[list[str]] = mapped_column(JSON, default=list)
    blocked_tools: Mapped[list[str]] = mapped_column(JSON, default=list)
    terminal_tools: Mapped[list[str]] = mapped_column(JSON, default=list)

    # Hooks (JSON object)
    hooks: Mapped[dict[str, Any] | None] = mapped_column(JSON, nullable=True)

    # Lifecycle
    background: Mapped[bool] = mapped_column(Boolean, default=False)
    initial_prompt: Mapped[str | None] = mapped_column(Text, nullable=True)

    # Team-mode fields
    role: Mapped[str | None] = mapped_column(String(64), nullable=True, index=True)
    agent_type: Mapped[str] = mapped_column(String(32), default="agent")
    supported_kinds: Mapped[list[str] | None] = mapped_column(JSON, nullable=True)
    source: Mapped[str] = mapped_column(String(32), default="user")

    # Capability flags
    can_spawn_subagents: Mapped[bool] = mapped_column(Boolean, default=True)
    require_fresh_client: Mapped[bool] = mapped_column(Boolean, default=False)
    include_skills: Mapped[bool] = mapped_column(Boolean, default=True)
    dispatchable_via_run_subagent: Mapped[bool] = mapped_column(Boolean, default=True)

    # Metadata & versioning
    version: Mapped[int] = mapped_column(Integer, default=1)
    is_active: Mapped[bool] = mapped_column(Boolean, default=True, index=True)
    created_by: Mapped[str | None] = mapped_column(String(128), nullable=True)
    tags: Mapped[list[str] | None] = mapped_column(JSON, nullable=True)
    metadata_json: Mapped[dict[str, Any] | None] = mapped_column("metadata", JSON, nullable=True)

    # Timestamps
    created_at: Mapped[datetime] = mapped_column(
        DateTime(timezone=True), default=lambda: datetime.now(UTC)
    )
    updated_at: Mapped[datetime] = mapped_column(
        DateTime(timezone=True),
        default=lambda: datetime.now(UTC),
        onupdate=lambda: datetime.now(UTC),
    )

    def __repr__(self) -> str:
        return f"<AgentDefinitionRecord name={self.name!r} v{self.version} active={self.is_active}>"
