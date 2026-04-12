"""SQLAlchemy ORM model for the ``task_notes`` table (Task Center).

See Section 14.4 of the coordination redesign doc for schema.
The table is partitioned by team_run_id (LIST partitioning).
"""

from __future__ import annotations

from datetime import datetime, timezone

from sqlalchemy import Text, DateTime
from sqlalchemy.dialects.postgresql import ARRAY, UUID
from sqlalchemy.orm import Mapped, mapped_column

from db.base import Base

import uuid as _uuid


def _utcnow() -> datetime:
    return datetime.now(timezone.utc)


def _genuuid() -> _uuid.UUID:
    return _uuid.uuid4()


class TaskNoteRecord(Base):
    """Durable record of a note in the Task Center."""

    __tablename__ = "task_notes"

    id: Mapped[_uuid.UUID] = mapped_column(
        UUID(as_uuid=True), primary_key=True, default=_genuuid
    )
    team_run_id: Mapped[str] = mapped_column(Text, primary_key=True)
    task_id: Mapped[str] = mapped_column(Text, nullable=False, index=True)
    agent_name: Mapped[str] = mapped_column(Text, nullable=False)
    content: Mapped[str] = mapped_column(Text, nullable=False)
    scope_ltree: Mapped[list[str]] = mapped_column(
        ARRAY(Text), default=list
    )
    created_at: Mapped[datetime] = mapped_column(
        DateTime(timezone=True), default=_utcnow
    )

    def __repr__(self) -> str:
        return (
            f"<TaskNoteRecord task={self.task_id!r} "
            f"agent={self.agent_name!r}>"
        )
