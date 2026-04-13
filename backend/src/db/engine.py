"""Database engine factory and session management.

Provides a single shared sync engine + optional async engine, initialised
once during application bootstrap. When no database is configured the helpers
return ``None`` so callers can fall back to file-based storage.
"""

from __future__ import annotations

import logging
import os
from typing import TYPE_CHECKING

from sqlalchemy import Engine, create_engine, inspect, text
from sqlalchemy.orm import Session, sessionmaker

from db.base import Base

if TYPE_CHECKING:
    from config.settings import DatabaseSettings
    from sqlalchemy.ext.asyncio import AsyncEngine, AsyncSession, async_sessionmaker

logger = logging.getLogger(__name__)

_engine: Engine | None = None
_session_factory: sessionmaker[Session] | None = None
_async_engine: "AsyncEngine | None" = None
_async_session_factory: "async_sessionmaker[AsyncSession] | None" = None

_INDEX_DDL: tuple[tuple[str, str, str], ...] = (
    (
        "token_usage",
        "ix_token_usage_run_id",
        'CREATE INDEX IF NOT EXISTS "ix_token_usage_run_id" ON "token_usage" ("run_id")',
    ),
)

def get_engine() -> Engine | None:
    """Return the shared engine (None if DB is not configured)."""
    return _engine


def get_session_factory() -> sessionmaker[Session] | None:
    """Return the shared sync session factory (None if DB is not configured)."""
    return _session_factory


def get_async_engine() -> "AsyncEngine | None":
    """Return the shared async engine (None if DB is not configured)."""
    return _async_engine


def get_async_session_factory() -> "async_sessionmaker[AsyncSession] | None":
    """Return the shared async session factory (None if DB is not configured)."""
    return _async_session_factory


def _add_missing_columns(engine: Engine) -> None:
    """Add columns that exist in ORM models but not yet in the database."""
    insp = inspect(engine)
    for table in Base.metadata.sorted_tables:
        if not insp.has_table(table.name):
            continue
        existing = {col["name"] for col in insp.get_columns(table.name)}
        for col in table.columns:
            if col.name not in existing:
                col_type = col.type.compile(dialect=engine.dialect)
                logger.info("Adding missing column %s.%s (%s)", table.name, col.name, col_type)
                with engine.begin() as conn:
                    conn.execute(
                        text(f'ALTER TABLE "{table.name}" ADD COLUMN "{col.name}" {col_type}')
                    )


def _ensure_indexes(engine: Engine) -> None:
    """Create indexes that may be missing on upgraded databases."""
    insp = inspect(engine)
    for table_name, index_name, ddl in _INDEX_DDL:
        if not insp.has_table(table_name):
            continue
        existing = {idx["name"] for idx in insp.get_indexes(table_name)}
        if index_name in existing:
            continue
        logger.info("Adding missing index %s on %s", index_name, table_name)
        with engine.begin() as conn:
            conn.execute(text(ddl))


def _async_database_url(url: str) -> "URL":
    """Convert a sync database URL to an async-compatible driver."""
    from sqlalchemy.engine import URL, make_url
    parsed = make_url(url)
    if parsed.drivername in {"postgresql+psycopg", "postgresql+asyncpg"}:
        return parsed
    if parsed.drivername in {"postgresql", "postgresql+psycopg2"}:
        return parsed.set(drivername="postgresql+psycopg")
    if parsed.drivername == "sqlite":
        return parsed.set(drivername="sqlite+aiosqlite")
    return parsed


def initialize_db(
    db_settings: "DatabaseSettings | None" = None,
) -> sessionmaker[Session] | None:
    """Create the engine, run DDL, and return a session factory.

    Sets up both sync and async engines from a single database URL.

    Args:
        db_settings: A ``DatabaseSettings`` instance.  When ``None`` or when
            ``db_settings.url`` is empty, falls back to the
            ``EPHEMERALOS_DATABASE_URL`` environment variable.

    Returns:
        A ``sessionmaker`` bound to the engine, or ``None`` when no URL is
        available.
    """
    global _engine, _session_factory, _async_engine, _async_session_factory

    url = (db_settings.url if db_settings and db_settings.url else None) or os.environ.get(
        "EPHEMERALOS_DATABASE_URL"
    )
    if not url:
        logger.info("No database URL configured — persistence disabled")
        return None

    pool_pre_ping = db_settings.pool_pre_ping if db_settings else True
    pool_size = db_settings.pool_size if db_settings else 5
    max_overflow = db_settings.max_overflow if db_settings else 10
    echo = db_settings.echo if db_settings else False

    logger.info("Connecting to database …")
    _engine = create_engine(
        url,
        pool_pre_ping=pool_pre_ping,
        pool_size=pool_size,
        max_overflow=max_overflow,
        echo=echo,
    )

    # Import models so Base.metadata knows about all tables
    import db.models  # noqa: F401

    Base.metadata.create_all(_engine)

    # Patch existing tables with columns added after initial creation.
    _add_missing_columns(_engine)
    _ensure_indexes(_engine)

    logger.info("Database tables created / verified")

    _session_factory = sessionmaker(bind=_engine, autoflush=False, expire_on_commit=False)

    # Create async engine from the same URL for DispatcherStore.
    import importlib.util
    if importlib.util.find_spec("greenlet") is not None:
        from sqlalchemy.ext.asyncio import (
            create_async_engine,
            async_sessionmaker as _asm,
        )
        _async_engine = create_async_engine(
            _async_database_url(url),
            pool_pre_ping=pool_pre_ping,
            pool_size=pool_size,
            max_overflow=max_overflow,
            echo=echo,
        )
        _async_session_factory = _asm(
            bind=_async_engine,
            autoflush=False,
            expire_on_commit=False,
        )
        logger.info("Async engine initialised")

    return _session_factory
