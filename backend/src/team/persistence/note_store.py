"""NoteStore — async Task Center persistence.

Follows the existing Store pattern (FileChangeStore, etc.) but uses
async_sessionmaker for non-blocking I/O in the asyncio executor.

See Section 14.2 of the coordination redesign doc.
"""

from __future__ import annotations

from datetime import datetime, timezone
import logging
from typing import Any

from sqlalchemy import select, text
from sqlalchemy.ext.asyncio import AsyncSession, async_sessionmaker

from team.persistence.ltree_utils import path_to_ltree
from team.persistence.task_note_record import TaskNoteRecord

logger = logging.getLogger(__name__)


class NoteStore:
    """Async Task Center persistence. Follows existing Store pattern."""

    def __init__(self) -> None:
        self._session_factory: async_sessionmaker[AsyncSession] | None = None

    def initialize(self, session_factory: async_sessionmaker[AsyncSession]) -> None:
        self._session_factory = session_factory
        logger.info("NoteStore initialised (async)")

    @property
    def initialized(self) -> bool:
        return self._session_factory is not None

    @property
    def _sf(self) -> async_sessionmaker[AsyncSession]:
        assert self._session_factory is not None, "NoteStore not initialised"
        return self._session_factory

    async def insert(self, note: TaskNoteRecord) -> None:
        """Insert a single note."""
        async with self._sf() as db:
            db.add(note)
            await db.commit()

    async def insert_batch(self, notes: list[TaskNoteRecord]) -> None:
        """Bulk insert notes. ON CONFLICT DO NOTHING for idempotency."""
        if not notes:
            return
        async with self._sf() as db:
            for note in notes:
                await db.merge(note)
            await db.commit()

    async def query_by_task_ids(
        self,
        run_id: str,
        task_ids: list[str],
    ) -> list[TaskNoteRecord]:
        """Fetch notes for specific tasks (dependency context)."""
        if not task_ids:
            return []
        async with self._sf() as db:
            stmt = (
                select(TaskNoteRecord)
                .where(
                    TaskNoteRecord.team_run_id == run_id,
                    TaskNoteRecord.task_id.in_(task_ids),
                )
                .order_by(TaskNoteRecord.created_at)
            )
            result = await db.execute(stmt)
            return list(result.scalars().all())

    async def query(
        self,
        run_id: str,
        *,
        task_ids: list[str] | None = None,
        scope_paths: list[str] | None = None,
        since: float | None = None,
        limit: int | None = None,
    ) -> list[TaskNoteRecord]:
        """Fetch notes with optional filters.

        PostgreSQL remains the source of truth; scope filtering is applied after
        the query so the caller sees the same semantics in both PG-backed and
        in-memory modes, including unscoped notes being visible to all queries.
        """
        async with self._sf() as db:
            stmt = select(TaskNoteRecord).where(TaskNoteRecord.team_run_id == run_id)
            if task_ids:
                stmt = stmt.where(TaskNoteRecord.task_id.in_(task_ids))
            if since is not None:
                stmt = stmt.where(
                    TaskNoteRecord.created_at
                    >= datetime.fromtimestamp(since, tz=timezone.utc)
                )
            stmt = stmt.order_by(TaskNoteRecord.created_at)
            result = await db.execute(stmt)
            rows = list(result.scalars().all())

        if scope_paths:
            normalized = [
                path_to_ltree(scope.rstrip("/"))
                for scope in scope_paths
                if isinstance(scope, str) and scope.strip()
            ]
            rows = [
                row
                for row in rows
                if not row.scope_ltree
                or any(
                    note_scope.startswith(query_scope)
                    for note_scope in row.scope_ltree
                    for query_scope in normalized
                )
            ]

        if limit is not None and limit > 0:
            rows = rows[-limit:]
        return rows

    async def search_fts(
        self,
        run_id: str,
        query: str,
        scope_ltrees: list[str] | None = None,
        limit: int = 10,
    ) -> list[Any]:
        """Full-text + optional ltree scope search.

        Uses text() for PG-specific tsvector and ltree operators.
        """
        async with self._sf() as db:
            result = await db.execute(
                text("""
                    SELECT task_id, agent_name, content, scope_ltree, created_at
                    FROM task_notes
                    WHERE team_run_id = :run_id
                      AND to_tsvector('english', content)
                          @@ plainto_tsquery('english', :query)
                      AND (:scopes::ltree[] IS NULL OR EXISTS (
                          SELECT 1 FROM unnest(scope_ltree) AS s
                          WHERE s <@ ANY(:scopes::ltree[])))
                    ORDER BY created_at DESC LIMIT :lim
                """),
                {
                    "run_id": run_id,
                    "query": query,
                    "scopes": scope_ltrees,
                    "lim": limit,
                },
            )
            return result.fetchall()


class NullNoteStore:
    """No-op store for when PostgreSQL is unavailable."""

    initialized: bool = False

    async def insert(self, note: Any) -> None:
        pass

    async def insert_batch(self, notes: list[Any]) -> None:
        pass

    async def query_by_task_ids(self, *args: Any, **kwargs: Any) -> list:
        return []

    async def query(self, *args: Any, **kwargs: Any) -> list:
        return []

    async def search_fts(self, *args: Any, **kwargs: Any) -> list:
        return []
