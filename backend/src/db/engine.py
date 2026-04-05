"""Database engine factory and session management.

Provides a single shared engine and session factory, initialised once during
application bootstrap.  When no database is configured the helpers return
``None`` so callers can fall back to file-based storage.
"""

from __future__ import annotations

import logging
import os
from typing import TYPE_CHECKING

from sqlalchemy import Engine, create_engine, inspect, text
from sqlalchemy.orm import Session, sessionmaker

from ephemeralos.db.base import Base

if TYPE_CHECKING:
    from ephemeralos.config.settings import DatabaseSettings

logger = logging.getLogger(__name__)

_engine: Engine | None = None
_session_factory: sessionmaker[Session] | None = None


def get_engine() -> Engine | None:
    """Return the shared engine (None if DB is not configured)."""
    return _engine


def get_session_factory() -> sessionmaker[Session] | None:
    """Return the shared session factory (None if DB is not configured)."""
    return _session_factory


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


def initialize_db(
    db_settings: DatabaseSettings | None = None,
) -> sessionmaker[Session] | None:
    """Create the engine, run DDL, and return a session factory.

    Args:
        db_settings: A ``DatabaseSettings`` instance.  When ``None`` or when
            ``db_settings.url`` is empty, falls back to the
            ``EPHEMERALOS_DATABASE_URL`` environment variable.

    Returns:
        A ``sessionmaker`` bound to the engine, or ``None`` when no URL is
        available.
    """
    global _engine, _session_factory

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
    import ephemeralos.db.models  # noqa: F401

    Base.metadata.create_all(_engine)

    # Patch existing tables with columns added after initial creation.
    # create_all only creates missing *tables*, not missing *columns*.
    _add_missing_columns(_engine)

    logger.info("Database tables created / verified")

    _session_factory = sessionmaker(bind=_engine, autoflush=False, expire_on_commit=False)
    return _session_factory
