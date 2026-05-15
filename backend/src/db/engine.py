"""Database engine factory and session management.

Provides a single shared sync engine, initialised once during application
bootstrap. When no database is configured the helpers return ``None`` so
callers can fall back to file-based storage.
"""

from __future__ import annotations

import logging
import os
from typing import TYPE_CHECKING, Any

from sqlalchemy import Engine, create_engine, inspect, text
from sqlalchemy.engine import make_url
from sqlalchemy.orm import Session, sessionmaker

from db.base import Base

if TYPE_CHECKING:
    from config.settings import DatabaseSettings

logger = logging.getLogger(__name__)

_engine: Engine | None = None
_session_factory: sessionmaker[Session] | None = None


def get_engine() -> Engine | None:
    """Return the shared engine (None if DB is not configured)."""
    return _engine


def get_session_factory() -> sessionmaker[Session] | None:
    """Return the shared sync session factory (None if DB is not configured)."""
    return _session_factory


_DROPPED_COLUMNS: dict[str, set[str]] = {
    "agent_runs": {
        "compacted_history",
        "event_count",
        "input_query",
        "metadata",
        "reasoning",
        "response",
        "session_id",
        "started_at",
        "status",
    },
    "task_center_tasks": {
        "acceptance_criteria",
        "children",
        "closes_for",
        "evaluator_id",
        "handoff_note",
        "parent_id",
        "run_id",
        "spec",
        "summary",
        "system_prompt",
        "title",
        "user_prompt",
    },
    "task_center_runs": {
        "root_task_id",
    },
}

_RENAMED_COLUMNS: dict[str, dict[str, str]] = {
    "task_center_tasks": {
        "run_id": "task_center_run_id",
    },
}


_LEGACY_TABLES_TO_DROP: set[str] = {
    "task_center_attempt",
}


def _drop_legacy_tables(engine: Engine) -> None:
    """Drop tables that are no longer modelled. Run after create_all/migrate."""
    insp = inspect(engine)
    for name in _LEGACY_TABLES_TO_DROP:
        if insp.has_table(name):
            logger.info("Dropping legacy table %s", name)
            with engine.begin() as conn:
                conn.execute(text(f'DROP TABLE IF EXISTS "{name}"'))


_LEGACY_TIER_TABLES: frozenset[str] = frozenset({"missions", "episodes", "trials"})


def init_db_with_legacy_check(engine: Engine) -> None:
    """Pre-create_all gate: refuse to start if pre-rename tier tables linger.

    Two prior renames stamped legacy table names that must be cleared from
    stale dev DBs: 2026-05-15 renamed `missions`/`episodes` to
    `goals`/`iterations`; 2026-05-16 renamed `trials` back to `attempts`.
    SQLAlchemy's `create_all` will create the new tables but leave the old
    ones intact, silently splitting state across two schemas. This gate
    detects that case and points the developer at the one-shot drop script.
    """
    insp = inspect(engine)
    present = _LEGACY_TIER_TABLES & set(insp.get_table_names())
    if present:
        raise RuntimeError(
            f"Legacy tier tables {sorted(present)} present after rename. "
            "Run: python -m backend.scripts.drop_legacy_tier_tables"
        )


def _drop_indexes_for_columns(engine: Engine, table_name: str, columns: set[str]) -> None:
    insp = inspect(engine)
    for index in insp.get_indexes(table_name):
        index_columns = set(index.get("column_names") or [])
        if not index_columns & columns:
            continue
        index_name = index["name"]
        logger.info("Dropping obsolete index %s on %s", index_name, table_name)
        with engine.begin() as conn:
            conn.execute(text(f'DROP INDEX IF EXISTS "{index_name}"'))


def _rebuild_sqlite_table(engine: Engine, table: Any) -> None:
    """Rebuild a SQLite table from ORM metadata, dropping legacy constraints."""
    table_name = table.name
    backup_name = f"__{table_name}_legacy"

    with engine.begin() as conn:
        existing = {col["name"] for col in inspect(conn).get_columns(table_name)}
        copy_columns = [col.name for col in table.columns if col.name in existing]
        required_columns = [
            col.name
            for col in table.columns
            if not col.nullable and col.default is None and col.server_default is None
        ]

        row_count = conn.execute(text(f'SELECT COUNT(*) FROM "{table_name}"')).scalar_one()
        invalid_required_rows = 0
        valid_required_filter = ""
        if row_count and required_columns:
            valid_required_filter = " AND ".join(
                f'"{col}" IS NOT NULL' for col in required_columns
            )
            null_checks = " OR ".join(f'"{col}" IS NULL' for col in required_columns)
            invalid_required_rows = conn.execute(
                text(f'SELECT COUNT(*) FROM "{table_name}" WHERE {null_checks}')
            ).scalar_one()

        for index in inspect(conn).get_indexes(table_name):
            conn.execute(text(f'DROP INDEX IF EXISTS "{index["name"]}"'))

        conn.execute(text(f'DROP TABLE IF EXISTS "{backup_name}"'))
        conn.execute(text(f'ALTER TABLE "{table_name}" RENAME TO "{backup_name}"'))
        table.create(conn)

        if copy_columns:
            column_sql = ", ".join(f'"{col}"' for col in copy_columns)
            where_sql = (
                f" WHERE {valid_required_filter}"
                if invalid_required_rows and valid_required_filter
                else ""
            )
            conn.execute(
                text(
                    f'INSERT INTO "{table_name}" ({column_sql}) '
                    f'SELECT {column_sql} FROM "{backup_name}"{where_sql}'
                )
            )
        if invalid_required_rows:
            logger.warning(
                "Skipping copy of %s incompatible legacy rows from %s",
                invalid_required_rows,
                table_name,
            )

        conn.execute(text(f'DROP TABLE "{backup_name}"'))


def _rename_columns(engine: Engine) -> None:
    """Rename known legacy columns before generic add/drop migration runs."""
    insp = inspect(engine)
    for table_name, renames in _RENAMED_COLUMNS.items():
        if not insp.has_table(table_name):
            continue
        existing = {col["name"] for col in insp.get_columns(table_name)}
        for old_name, new_name in renames.items():
            if old_name not in existing:
                continue
            if new_name in existing:
                with engine.begin() as conn:
                    conn.execute(
                        text(
                            f'UPDATE "{table_name}" '
                            f'SET "{new_name}" = "{old_name}" '
                            f'WHERE "{new_name}" IS NULL'
                        )
                    )
                continue
            logger.info(
                "Renaming legacy column %s.%s to %s",
                table_name,
                old_name,
                new_name,
            )
            with engine.begin() as conn:
                conn.execute(
                    text(
                        f'ALTER TABLE "{table_name}" '
                        f'RENAME COLUMN "{old_name}" TO "{new_name}"'
                    )
                )


def _add_missing_columns(engine: Engine) -> None:
    """Add columns from the ORM, drop columns no longer in the model."""
    insp = inspect(engine)
    for table in Base.metadata.sorted_tables:
        if not insp.has_table(table.name):
            continue
        existing = {col["name"] for col in insp.get_columns(table.name)}
        stale_columns = _DROPPED_COLUMNS.get(table.name, set()) & existing
        if stale_columns:
            _drop_indexes_for_columns(engine, table.name, stale_columns)
        for col in table.columns:
            if col.name not in existing:
                col_type = col.type.compile(dialect=engine.dialect)
                logger.info("Adding missing column %s.%s (%s)", table.name, col.name, col_type)
                with engine.begin() as conn:
                    conn.execute(
                        text(f'ALTER TABLE "{table.name}" ADD COLUMN "{col.name}" {col_type}')
                    )
                    if table.name == "task_center_tasks" and col.name == "rendered_prompt":
                        if "spec" in existing:
                            conn.execute(
                                text(
                                    'UPDATE "task_center_tasks" '
                                    'SET "rendered_prompt" = "spec" '
                                    'WHERE "rendered_prompt" IS NULL'
                                )
                            )
        for stale in stale_columns:
            logger.info("Dropping obsolete column %s.%s", table.name, stale)
        if stale_columns:
            if engine.dialect.name == "sqlite":
                _rebuild_sqlite_table(engine, table)
            else:
                for stale in stale_columns:
                    with engine.begin() as conn:
                        conn.execute(text(f'ALTER TABLE "{table.name}" DROP COLUMN "{stale}"'))


def initialize_db(
    db_settings: "DatabaseSettings | None" = None,
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
    engine_kwargs: dict[str, Any] = {"echo": echo}
    is_sqlite = make_url(url).drivername.startswith("sqlite")
    if not is_sqlite:
        engine_kwargs.update(
            pool_pre_ping=pool_pre_ping,
            pool_size=pool_size,
            max_overflow=max_overflow,
        )
    _engine = create_engine(url, **engine_kwargs)

    # Import models so Base.metadata knows about all tables
    import db.models  # noqa: F401

    # Refuse to proceed if pre-rename tier tables linger from a stale dev DB.
    init_db_with_legacy_check(_engine)

    Base.metadata.create_all(_engine)

    # Rename legacy columns before patching missing/new columns.
    _rename_columns(_engine)

    # Patch existing tables with columns added after initial creation.
    _add_missing_columns(_engine)

    # Drop tables that are no longer modelled (post-migration cleanup).
    _drop_legacy_tables(_engine)

    logger.info("Database tables created / verified")

    _session_factory = sessionmaker(bind=_engine, autoflush=False, expire_on_commit=False)

    return _session_factory
