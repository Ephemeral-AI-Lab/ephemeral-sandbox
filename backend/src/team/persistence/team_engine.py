"""Async database bootstrap for team coordination stores.

Delegates engine creation to db.engine which manages both sync and async
engines. This module handles team-specific concerns: registering team
ORM models, creating team tables, and migrating legacy ltree columns.
"""

from __future__ import annotations

import logging
from typing import TYPE_CHECKING

from sqlalchemy import text
from sqlalchemy.engine import Engine

from db.base import Base
from db.engine import (
    _add_missing_columns,
    _ensure_indexes,
    get_async_engine,
    get_async_session_factory,
    get_engine,
    get_session_factory,
    initialize_db,
)

if TYPE_CHECKING:
    from config.settings import Settings
    from sqlalchemy.ext.asyncio import AsyncEngine, AsyncSession, async_sessionmaker

logger = logging.getLogger(__name__)

_LEGACY_LTREE_COLUMNS: tuple[tuple[str, str, str, str], ...] = (
    (
        "tasks",
        "scope_ltree",
        "ltree[]",
        "ALTER TABLE tasks ALTER COLUMN scope_ltree TYPE TEXT[] "
        "USING COALESCE(scope_ltree::text[], ARRAY[]::text[])",
    ),
)


def _ensure_team_models_registered() -> None:
    """Import team ORM models so Base.metadata knows about them."""
    from team.persistence.task_record import TaskRecord  # noqa: F401


def _legacy_column_type(engine: Engine, table_name: str, column_name: str) -> str | None:
    """Check the current column type in PG catalog."""
    with engine.begin() as conn:
        return conn.execute(
            text(
                """
                SELECT pg_catalog.format_type(a.atttypid, a.atttypmod)
                FROM pg_attribute a
                WHERE a.attrelid = to_regclass(:table_name)
                  AND a.attname = :column_name
                  AND a.attnum > 0
                  AND NOT a.attisdropped
                """
            ),
            {"table_name": table_name, "column_name": column_name},
        ).scalar()


def _normalize_legacy_ltree_columns(engine: Engine) -> None:
    """Migrate legacy ltree columns to plain TEXT storage."""
    if engine.dialect.name != "postgresql":
        return
    for table_name, column_name, legacy_type, ddl in _LEGACY_LTREE_COLUMNS:
        if _legacy_column_type(engine, table_name, column_name) != legacy_type:
            continue
        logger.info(
            "Converting legacy column %s.%s from %s to TEXT storage",
            table_name,
            column_name,
            legacy_type,
        )
        with engine.begin() as conn:
            conn.execute(text(ddl))


def get_team_engine() -> "AsyncEngine | None":
    """Return the shared async engine."""
    return get_async_engine()


def get_team_session_factory() -> "async_sessionmaker[AsyncSession] | None":
    """Return the shared async session factory."""
    return get_async_session_factory()


def create_team_engine(
    settings: "Settings | None" = None,
) -> "tuple[AsyncEngine, async_sessionmaker[AsyncSession]]":
    """Ensure the team coordination tables exist and return the async engine.

    Registers team ORM models, creates tables, and migrates legacy columns.
    Delegates engine creation to db.engine.initialize_db.
    """
    factory = get_async_session_factory()
    engine = get_async_engine()
    if factory is not None and engine is not None:
        return engine, factory

    # Ensure sync+async engines exist.
    if get_session_factory() is None:
        if settings is not None:
            initialize_db(settings.database)
        else:
            from config.settings import load_settings
            initialize_db(load_settings().database)

    sync_engine = get_engine()
    if sync_engine is None:
        raise RuntimeError("Team runtime requires a configured database.")

    # Register team models and create their tables.
    _ensure_team_models_registered()
    Base.metadata.create_all(sync_engine)
    _normalize_legacy_ltree_columns(sync_engine)
    _add_missing_columns(sync_engine)
    _ensure_indexes(sync_engine)

    engine = get_async_engine()
    factory = get_async_session_factory()
    if engine is None or factory is None:
        raise RuntimeError(
            "Team runtime requires an async database engine. "
            "Ensure greenlet is installed and EPHEMERALOS_DATABASE_URL is set."
        )

    logger.info("Team async engine ready")
    return engine, factory
