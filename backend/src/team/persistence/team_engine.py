"""Async SQLAlchemy engine for team coordination.

Coexists with the sync engine in db/engine.py (used by SessionStore,
AgentRunStore, etc.). This engine uses asyncpg as the async driver for
LISTEN/NOTIFY support and non-blocking I/O in the asyncio executor.

See Section 14.2 of the coordination redesign doc.
"""

from __future__ import annotations

import logging
import os

from sqlalchemy.ext.asyncio import (
    AsyncEngine,
    AsyncSession,
    async_sessionmaker,
    create_async_engine,
)

logger = logging.getLogger(__name__)

_engine: AsyncEngine | None = None
_session_factory: async_sessionmaker[AsyncSession] | None = None


def get_team_engine() -> AsyncEngine | None:
    return _engine


def get_team_session_factory() -> async_sessionmaker[AsyncSession] | None:
    return _session_factory


def create_team_engine(
    max_agents: int = 8,
    database_url: str | None = None,
) -> tuple[AsyncEngine, async_sessionmaker[AsyncSession]]:
    """Create the async engine for team coordination.

    Pool budget: max_agents (query connections)
               + 1 (shared LISTEN/NOTIFY connection)
               + 4 (headroom for dispatcher, cache, health checks)
    """
    global _engine, _session_factory

    url = database_url or os.environ.get("EPHEMERALOS_DATABASE_URL", "")
    if not url:
        raise RuntimeError(
            "No database URL configured. Set EPHEMERALOS_DATABASE_URL or "
            "pass database_url to create_team_engine()."
        )

    # Convert sync URL to async (asyncpg driver)
    async_url = url.replace("postgresql://", "postgresql+asyncpg://")
    if "postgresql+asyncpg" not in async_url:
        async_url = f"postgresql+asyncpg://{async_url.split('://', 1)[-1]}"

    pool_size = max_agents + 5
    logger.info(
        "Creating team async engine (pool_size=%d, max_overflow=4)",
        pool_size,
    )

    _engine = create_async_engine(
        async_url,
        pool_size=pool_size,
        max_overflow=4,
        pool_pre_ping=True,
        pool_recycle=300,
    )
    _session_factory = async_sessionmaker(
        _engine,
        expire_on_commit=False,
    )
    return _engine, _session_factory
