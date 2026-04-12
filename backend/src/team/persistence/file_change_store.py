"""FileChangeStore — durable file-change persistence for cross-run history.

Dual-write companion to the in-memory Ledger. The Ledger handles hot-path
reads (context_for, same-run scope queries). This store handles:

  1. Cross-run contention history (query_edit_history for planner)
  2. Multi-process visibility (edits by process A visible to process B)
  3. Crash recovery (edit history survives process restart)

Follows the existing Store pattern (TeamDefinitionStore, etc.):
sync SQLAlchemy sessionmaker, initialize() contract, null fallback.
"""

from __future__ import annotations

import logging
import time
from dataclasses import dataclass
from datetime import datetime, timezone
from typing import Any

from sqlalchemy import DateTime, Float, String, Text, BigInteger, text
from sqlalchemy.orm import Mapped, Session, mapped_column, sessionmaker

from db.base import Base

logger = logging.getLogger(__name__)


def _utcnow() -> datetime:
    return datetime.now(timezone.utc)


# ---------------------------------------------------------------------------
# ORM Model
# ---------------------------------------------------------------------------


class FileChangeRecord(Base):
    """Durable record of a file edit by an agent."""

    __tablename__ = "file_changes"

    id: Mapped[int] = mapped_column(BigInteger, primary_key=True, autoincrement=True)
    team_run_id: Mapped[str] = mapped_column(String(64), index=True)
    file_path: Mapped[str] = mapped_column(Text, nullable=False)
    agent_id: Mapped[str] = mapped_column(String(64), nullable=False)
    agent_run_id: Mapped[str] = mapped_column(String(64), default="")
    edit_type: Mapped[str] = mapped_column(String(32), default="edit")
    old_hash: Mapped[str] = mapped_column(String(64), default="")
    new_hash: Mapped[str] = mapped_column(String(64), default="")
    description: Mapped[str] = mapped_column(Text, default="")
    timestamp: Mapped[float] = mapped_column(Float, default=time.time)
    created_at: Mapped[datetime] = mapped_column(
        DateTime(timezone=True), default=_utcnow
    )

    def __repr__(self) -> str:
        return (
            f"<FileChangeRecord {self.file_path!r} "
            f"by={self.agent_id!r} type={self.edit_type!r}>"
        )


# ---------------------------------------------------------------------------
# Contention hotspot result
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class ContentionHotspot:
    file_path: str
    agent_count: int
    edit_count: int


# ---------------------------------------------------------------------------
# Store
# ---------------------------------------------------------------------------


class FileChangeStore:
    """Durable file-change persistence. Sync SQLAlchemy, existing Store pattern."""

    def __init__(self) -> None:
        self._session_factory: sessionmaker[Session] | None = None

    def initialize(self, session_factory: sessionmaker[Session]) -> None:
        self._session_factory = session_factory
        logger.info("FileChangeStore initialised")

    @property
    def initialized(self) -> bool:
        return self._session_factory is not None

    @property
    def _sf(self) -> sessionmaker[Session]:
        assert self._session_factory is not None, "FileChangeStore not initialised"
        return self._session_factory

    def record(
        self,
        *,
        team_run_id: str,
        file_path: str,
        agent_id: str,
        agent_run_id: str = "",
        edit_type: str = "edit",
        old_hash: str = "",
        new_hash: str = "",
        description: str = "",
    ) -> FileChangeRecord:
        """Insert a file change record."""
        record = FileChangeRecord(
            team_run_id=team_run_id,
            file_path=file_path,
            agent_id=agent_id,
            agent_run_id=agent_run_id,
            edit_type=edit_type,
            old_hash=old_hash,
            new_hash=new_hash,
            description=description,
            timestamp=time.time(),
        )
        with self._sf() as session:
            session.add(record)
            session.commit()
        return record

    def changes_in_scope(
        self,
        team_run_id: str,
        scope_prefixes: list[str],
        since: float,
    ) -> list[FileChangeRecord]:
        """Return file changes under scope prefixes since a timestamp."""
        if not scope_prefixes:
            return []
        with self._sf() as session:
            # Build OR conditions for prefix matching
            conditions = " OR ".join(
                f"file_path LIKE :prefix_{i}" for i in range(len(scope_prefixes))
            )
            params: dict[str, Any] = {
                "run_id": team_run_id,
                "since": since,
            }
            for i, prefix in enumerate(scope_prefixes):
                params[f"prefix_{i}"] = prefix.rstrip("/") + "%"

            result = session.execute(
                text(f"""
                    SELECT id, team_run_id, file_path, agent_id, agent_run_id,
                           edit_type, old_hash, new_hash, description, timestamp, created_at
                    FROM file_changes
                    WHERE team_run_id = :run_id
                      AND timestamp > :since
                      AND ({conditions})
                    ORDER BY timestamp DESC
                """),
                params,
            )
            return [self._row_to_record(row) for row in result.fetchall()]

    def external_changes_in_scope(
        self,
        team_run_id: str,
        scope_prefixes: list[str],
        since: float,
        exclude_run_id: str | None = None,
    ) -> list[FileChangeRecord]:
        """Return changes in scope NOT made by agents in this team run."""
        changes = self.changes_in_scope(team_run_id, scope_prefixes, since)
        if exclude_run_id:
            changes = [c for c in changes if c.agent_run_id != exclude_run_id]
        return changes

    def contention_hotspots(
        self,
        scope_prefixes: list[str],
        limit: int = 10,
    ) -> list[ContentionHotspot]:
        """Cross-run contention hotspots: files edited by many agents.

        Used by planner's query_edit_history tool to predict conflicts."""
        if not scope_prefixes:
            return []
        with self._sf() as session:
            conditions = " OR ".join(
                f"file_path LIKE :prefix_{i}" for i in range(len(scope_prefixes))
            )
            params: dict[str, Any] = {"lim": limit}
            for i, prefix in enumerate(scope_prefixes):
                params[f"prefix_{i}"] = prefix.rstrip("/") + "%"

            result = session.execute(
                text(f"""
                    SELECT file_path,
                           COUNT(DISTINCT agent_id) AS agent_count,
                           COUNT(*) AS edit_count
                    FROM file_changes
                    WHERE {conditions}
                    GROUP BY file_path
                    HAVING COUNT(DISTINCT agent_id) > 1
                    ORDER BY agent_count DESC, edit_count DESC
                    LIMIT :lim
                """),
                params,
            )
            return [
                ContentionHotspot(
                    file_path=row.file_path,
                    agent_count=row.agent_count,
                    edit_count=row.edit_count,
                )
                for row in result.fetchall()
            ]

    @staticmethod
    def _row_to_record(row: Any) -> FileChangeRecord:
        """Convert a raw SQL row to a FileChangeRecord."""
        return FileChangeRecord(
            id=row.id,
            team_run_id=row.team_run_id,
            file_path=row.file_path,
            agent_id=row.agent_id,
            agent_run_id=row.agent_run_id,
            edit_type=row.edit_type,
            old_hash=row.old_hash,
            new_hash=row.new_hash,
            description=row.description,
            timestamp=row.timestamp,
            created_at=row.created_at,
        )


# ---------------------------------------------------------------------------
# Null fallback (no-PG / tests)
# ---------------------------------------------------------------------------


class NullFileChangeStore:
    """No-op store for when PostgreSQL is unavailable."""

    initialized: bool = False

    def record(self, **kwargs: Any) -> None:
        pass

    def changes_in_scope(self, *args: Any, **kwargs: Any) -> list:
        return []

    def external_changes_in_scope(self, *args: Any, **kwargs: Any) -> list:
        return []

    def contention_hotspots(self, *args: Any, **kwargs: Any) -> list:
        return []
