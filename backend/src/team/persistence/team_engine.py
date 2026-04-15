"""Async database bootstrap for team coordination stores.

Delegates engine creation to db.engine which manages both sync and async
engines. This module handles team-specific concerns: registering team
ORM models, creating team tables, and rejecting unsupported legacy schemas.
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

_UNSUPPORTED_LEGACY_COLUMNS: tuple[tuple[str, str, str, str], ...] = (
    (
        "tasks",
        "scope_ltree",
        "ltree[]",
        "Legacy tasks.scope_ltree uses ltree[] storage. "
        "EphemeralOS now requires TEXT[] scope_ltree columns; "
        "migrate or recreate the tasks table before startup.",
    ),
    (
        "tasks",
        "task",
        "text",
        "Legacy tasks.task columns are no longer auto-migrated. "
        "Backfill objective/description and drop the legacy task column "
        "before starting EphemeralOS.",
    ),
)


def _ensure_team_models_registered() -> None:
    """Import team ORM models so Base.metadata knows about them."""
    from team.persistence.task_record import TaskRecord  # noqa: F401
    from team.persistence.blocker_record import BlockerRecord  # noqa: F401


def _legacy_column_type(engine: Engine, table_name: str, column_name: str) -> str | None:
    """Check the current column type via pg_catalog.format_type (PostgreSQL)."""
    with engine.begin() as conn:
        return conn.execute(
            text(
                "SELECT pg_catalog.format_type(a.atttypid, a.atttypmod) "
                "FROM pg_attribute a "
                "WHERE a.attrelid = to_regclass(:table_name) "
                "AND a.attname = :column_name "
                "AND a.attnum > 0 "
                "AND NOT a.attisdropped"
            ),
            {"table_name": table_name, "column_name": column_name},
        ).scalar()


def _reject_unsupported_legacy_columns(engine: Engine) -> None:
    """Fail fast when the database still uses unsupported legacy team columns."""
    if engine.dialect.name != "postgresql":
        return
    for table_name, column_name, legacy_type, message in _UNSUPPORTED_LEGACY_COLUMNS:
        if _legacy_column_type(engine, table_name, column_name) != legacy_type:
            continue
        raise RuntimeError(
            f"Unsupported legacy schema detected at {table_name}.{column_name}: {message}"
        )


def _ensure_team_schema(engine: Engine) -> None:
    """Register team models and backfill any missing team columns/indexes."""
    _ensure_team_models_registered()
    Base.metadata.create_all(engine)
    _reject_unsupported_legacy_columns(engine)
    _add_missing_columns(engine)
    _ensure_indexes(engine)


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

    Registers team ORM models, creates tables, and validates the live schema.
    Delegates engine creation to db.engine.initialize_db.
    """
    factory = get_async_session_factory()
    engine = get_async_engine()
    sync_engine = get_engine()
    if factory is not None and engine is not None and sync_engine is not None:
        _ensure_team_schema(sync_engine)
        return engine, factory

    # Ensure sync+async engines exist.
    if get_session_factory() is None:
        if settings is not None:
            initialize_db(settings.database)
        else:
            from config.settings import load_settings

            initialize_db(load_settings().database)

    if sync_engine is None:
        raise RuntimeError("Team runtime requires a configured database.")

    _ensure_team_schema(sync_engine)

    engine = get_async_engine()
    factory = get_async_session_factory()
    if engine is None or factory is None:
        raise RuntimeError(
            "Team runtime requires an async database engine. "
            "Ensure greenlet is installed and EPHEMERALOS_DATABASE_URL is set."
        )

    logger.info("Team async engine ready")
    return engine, factory
