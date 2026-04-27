"""Database engine bootstrap and lightweight schema migration tests."""

from __future__ import annotations

import importlib.util
from importlib.machinery import ModuleSpec
from collections.abc import Iterator
from pathlib import Path

import pytest
from sqlalchemy import create_engine, inspect, text

import db.engine as engine_mod
from config.settings import DatabaseSettings
from db.stores.agent_run_store import AgentRunStore
from db.stores.task_center_store import TaskCenterStore


@pytest.fixture(autouse=True)
def reset_db_engine_state() -> Iterator[None]:
    _reset_db_engine_state()
    try:
        yield
    finally:
        _reset_db_engine_state()


def _reset_db_engine_state() -> None:
    if engine_mod._engine is not None:
        engine_mod._engine.dispose()
    engine_mod._engine = None
    engine_mod._session_factory = None
    engine_mod._async_engine = None
    engine_mod._async_session_factory = None


def _hide_aiosqlite(monkeypatch: pytest.MonkeyPatch) -> None:
    real_find_spec = importlib.util.find_spec

    def fake_find_spec(name: str, package: str | None = None) -> ModuleSpec | None:
        if name == "aiosqlite":
            return None
        return real_find_spec(name, package)

    monkeypatch.setattr(importlib.util, "find_spec", fake_find_spec)


def test_initialize_db_skips_missing_sqlite_async_driver(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    _hide_aiosqlite(monkeypatch)

    import sqlalchemy.ext.asyncio as sa_async

    def fail_create_async_engine(*args: object, **kwargs: object) -> object:
        del args, kwargs
        raise AssertionError("async engine should be skipped without aiosqlite")

    monkeypatch.setattr(sa_async, "create_async_engine", fail_create_async_engine)

    db_path = tmp_path / "runtime.db"
    sf = engine_mod.initialize_db(DatabaseSettings(url=f"sqlite:///{db_path}"))

    assert sf is not None
    assert engine_mod.get_async_session_factory() is None
    engine = engine_mod.get_engine()
    assert engine is not None
    assert inspect(engine).has_table("task_center_tasks")


def test_initialize_db_migrates_legacy_agent_runs_schema(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    _hide_aiosqlite(monkeypatch)
    db_path = tmp_path / "legacy.db"
    legacy_engine = create_engine(f"sqlite:///{db_path}")
    with legacy_engine.begin() as conn:
        conn.execute(
            text(
                """
                CREATE TABLE sessions (
                    id VARCHAR(36) NOT NULL,
                    PRIMARY KEY (id)
                )
                """
            )
        )
        conn.execute(
            text(
                """
                CREATE TABLE agent_runs (
                    id VARCHAR(36) NOT NULL,
                    session_id VARCHAR(36) NOT NULL,
                    agent_name VARCHAR(128) NOT NULL,
                    status VARCHAR(32) NOT NULL,
                    input_query TEXT,
                    response JSON,
                    message_history JSON,
                    compacted_history JSON,
                    reasoning TEXT,
                    error TEXT,
                    event_count INTEGER NOT NULL,
                    metadata JSON,
                    started_at DATETIME,
                    finished_at DATETIME,
                    created_at DATETIME NOT NULL,
                    PRIMARY KEY (id),
                    FOREIGN KEY(session_id) REFERENCES sessions (id) ON DELETE CASCADE
                )
                """
            )
        )
        conn.execute(text("CREATE INDEX ix_agent_runs_session_id ON agent_runs (session_id)"))
    legacy_engine.dispose()

    sf = engine_mod.initialize_db(DatabaseSettings(url=f"sqlite:///{db_path}"))
    assert sf is not None

    engine = engine_mod.get_engine()
    assert engine is not None
    insp = inspect(engine)
    agent_columns = {col["name"] for col in insp.get_columns("agent_runs")}
    assert "session_id" not in agent_columns
    assert "status" not in agent_columns
    assert {"task_id", "terminal_tool_result", "token_count"} <= agent_columns
    assert not any(index["name"] == "ix_agent_runs_session_id" for index in insp.get_indexes("agent_runs"))

    task_center_store = TaskCenterStore()
    task_center_store.initialize(sf)
    agent_run_store = AgentRunStore()
    agent_run_store.initialize(sf)

    task_center_store.create_request(
        request_id="req",
        cwd="/repo",
        sandbox_id=None,
        request_prompt="prompt",
    )
    task_center_store.create_run(run_id="run", request_id="req")
    task_center_store.upsert_task(
        task_id="run:t1",
        run_id="run",
        role="executor",
        task_input="prompt",
        status="running",
        summaries=[],
        needs=[],
        task_center_harness_graph_id=None,
    )

    agent_run_store.create_run(
        run_id="agent1",
        task_id="run:t1",
        agent_name="executor",
    )

    assert agent_run_store.get_run("agent1") is not None
