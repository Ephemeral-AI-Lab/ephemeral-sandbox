"""Persistence model for :class:`ContextPacket`.

Packets are write-once: helpers fetch by id and read-only. The schema mirrors
the pydantic ``ContextPacket`` shape, with collections serialized as JSON.
"""

from __future__ import annotations

from datetime import UTC, datetime

from sqlalchemy import JSON, DateTime, String
from sqlalchemy.orm import Mapped, mapped_column

from db.base import Base


class ContextPacketRecord(Base):
    """Immutable persisted view of a :class:`ContextPacket`."""

    __tablename__ = "context_packets"

    id: Mapped[str] = mapped_column(String(36), primary_key=True)
    target_role: Mapped[str] = mapped_column(String(32))
    target_id: Mapped[str | None] = mapped_column(String(255), nullable=True)
    canonical_refs: Mapped[dict] = mapped_column(JSON)
    blocks: Mapped[list] = mapped_column(JSON, default=list)
    metadata_payload: Mapped[dict] = mapped_column(
        "metadata", JSON, default=dict
    )
    source_ids: Mapped[list] = mapped_column(JSON, default=list)
    created_at: Mapped[datetime] = mapped_column(
        DateTime(timezone=True), default=lambda: datetime.now(UTC)
    )

    def __repr__(self) -> str:  # pragma: no cover
        return (
            f"<ContextPacketRecord id={self.id!r} role={self.target_role!r}>"
        )
