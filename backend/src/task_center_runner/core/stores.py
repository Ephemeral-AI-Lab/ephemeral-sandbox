"""Per-test database isolation for live e2e TaskCenter stores.

Reuses the project's shared SQLAlchemy engine via ``db.engine.initialize_db()``
and carves a fresh isolated store per test so concurrent tests do not collide.

PostgreSQL uses a per-test schema routed through ``schema_translate_map``.
SQLite uses a per-test database file next to the configured SQLite database.
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
from uuid import uuid4

from sqlalchemy import Engine, MetaData, create_engine, event, text
from sqlalchemy.orm import Session, sessionmaker

from db.base import Base
import db.models  # noqa: F401 — populate SQLAlchemy metadata
from db.engine import get_engine, initialize_db
from db.stores.attempt_store import AttemptStore
from db.stores.context_packet_store import ContextPacketStore
from db.stores.iteration_store import IterationStore
from db.stores.workflow_store import WorkflowStore
from db.stores.task_center_store import TaskCenterStore


@dataclass(slots=True)
class TaskCenterStoreBundle:
    """Bundle of TaskCenter stores bound to an isolated test database."""

    engine: Engine
    schema: str
    session_factory: sessionmaker[Session]
    task_store: TaskCenterStore
    workflow_store: WorkflowStore
    iteration_store: IterationStore
    attempt_store: AttemptStore
    context_packet_store: ContextPacketStore
    owns_engine: bool = False
    cleanup_paths: tuple[Path, ...] = ()

    def close(self) -> None:
        """Release per-test database resources."""
        if self.engine.dialect.name == "postgresql":
            with self.engine.begin() as conn:
                conn.execute(text(f'DROP SCHEMA IF EXISTS "{self.schema}" CASCADE'))
            return

        if self.owns_engine:
            self.engine.dispose()
        for path in self.cleanup_paths:
            for candidate in (
                path,
                Path(f"{path}-wal"),
                Path(f"{path}-shm"),
                Path(f"{path}-journal"),
            ):
                try:
                    candidate.unlink()
                except FileNotFoundError:
                    pass


def _ensure_initialized() -> Engine:
    """Bootstrap the shared engine when needed."""
    engine = get_engine()
    if engine is None:
        initialize_db()
        engine = get_engine()
    if engine is None:
        raise RuntimeError(
            "database URL not configured — set database.url in ephemeralos.yaml "
            "or export an explicit database override before running "
            "task_center_runner tests."
        )
    return engine


def _initialize_bundle(
    *,
    engine: Engine,
    schema: str,
    session_factory: sessionmaker[Session],
    owns_engine: bool = False,
    cleanup_paths: tuple[Path, ...] = (),
) -> TaskCenterStoreBundle:
    bundle = TaskCenterStoreBundle(
        engine=engine,
        schema=schema,
        session_factory=session_factory,
        task_store=TaskCenterStore(),
        workflow_store=WorkflowStore(),
        iteration_store=IterationStore(),
        attempt_store=AttemptStore(),
        context_packet_store=ContextPacketStore(),
        owns_engine=owns_engine,
        cleanup_paths=cleanup_paths,
    )
    for store in (
        bundle.task_store,
        bundle.workflow_store,
        bundle.iteration_store,
        bundle.attempt_store,
        bundle.context_packet_store,
    ):
        store.initialize(session_factory)
    return bundle


def _create_postgresql_bundle(
    shared_engine: Engine, *, schema_prefix: str
) -> TaskCenterStoreBundle:
    """Carve a fresh PostgreSQL schema and route ORM calls into it."""
    schema = f"{schema_prefix}_{uuid4().hex[:12]}"

    with shared_engine.begin() as conn:
        conn.execute(text(f'CREATE SCHEMA "{schema}"'))

    test_metadata = MetaData(schema=schema)
    for table in Base.metadata.sorted_tables:
        table.to_metadata(test_metadata, schema=schema)
    test_metadata.create_all(shared_engine)

    routed_engine = shared_engine.execution_options(
        schema_translate_map={None: schema}
    )
    session_factory = sessionmaker(
        bind=routed_engine, autoflush=False, expire_on_commit=False
    )
    return _initialize_bundle(
        engine=routed_engine,
        schema=schema,
        session_factory=session_factory,
    )


def _sqlite_bundle_path(shared_engine: Engine, schema: str) -> Path | None:
    database = shared_engine.url.database
    if not database or database == ":memory:":
        return None
    base_path = Path(database).expanduser().resolve()
    bundle_dir = base_path.parent / "task_center_runner"
    bundle_dir.mkdir(parents=True, exist_ok=True)
    return bundle_dir / f"{schema}.db"


def _create_sqlite_bundle(
    shared_engine: Engine, *, schema_prefix: str
) -> TaskCenterStoreBundle:
    """Create an isolated SQLite database file for one test bundle."""
    schema = f"{schema_prefix}_{uuid4().hex[:12]}"
    db_path = _sqlite_bundle_path(shared_engine, schema)
    if db_path is None:
        sqlite_url = "sqlite:///:memory:"
        cleanup_paths: tuple[Path, ...] = ()
    else:
        sqlite_url = f"sqlite:///{db_path}"
        cleanup_paths = (db_path,)

    engine = create_engine(sqlite_url, echo=shared_engine.echo)

    @event.listens_for(engine, "connect")
    def _configure_sqlite(dbapi_connection: object, _: object) -> None:
        cursor = dbapi_connection.cursor()
        try:
            cursor.execute("PRAGMA foreign_keys=ON")
            cursor.execute("PRAGMA busy_timeout=30000")
            cursor.execute("PRAGMA journal_mode=WAL")
        finally:
            cursor.close()

    Base.metadata.create_all(engine)
    session_factory = sessionmaker(bind=engine, autoflush=False, expire_on_commit=False)
    return _initialize_bundle(
        engine=engine,
        schema=schema,
        session_factory=session_factory,
        owns_engine=True,
        cleanup_paths=cleanup_paths,
    )


def create_per_test_task_center_stores(
    *, schema_prefix: str = "task_center_runner"
) -> TaskCenterStoreBundle:
    """Return isolated TaskCenter stores for the configured database dialect."""
    shared_engine = _ensure_initialized()
    if shared_engine.dialect.name == "postgresql":
        return _create_postgresql_bundle(shared_engine, schema_prefix=schema_prefix)
    if shared_engine.dialect.name == "sqlite":
        return _create_sqlite_bundle(shared_engine, schema_prefix=schema_prefix)
    raise RuntimeError(
        "task_center_runner supports PostgreSQL or SQLite, "
        f"got dialect={shared_engine.dialect.name!r}"
    )


__all__ = [
    "TaskCenterStoreBundle",
    "create_per_test_task_center_stores",
]
