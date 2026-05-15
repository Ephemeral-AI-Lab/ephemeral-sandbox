"""Database engine bootstrap and lightweight schema migration tests."""

from __future__ import annotations

from collections.abc import Iterator
from pathlib import Path

import pytest
from sqlalchemy import create_engine, inspect, text

import db.engine as engine_mod
import db.models  # noqa: F401 - populate Base.metadata
from config.settings import DatabaseSettings
from db.base import Base
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


def test_initialize_db_renames_task_center_child_run_id_columns(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    db_path = tmp_path / "legacy-task-center.db"
    legacy_engine = create_engine(f"sqlite:///{db_path}")
    with legacy_engine.begin() as conn:
        conn.execute(
            text(
                """
                CREATE TABLE task_center_requests (
                    id VARCHAR(36) NOT NULL,
                    cwd VARCHAR(1024) NOT NULL,
                    sandbox_id VARCHAR(128),
                    request_prompt TEXT NOT NULL,
                    created_at DATETIME NOT NULL,
                    updated_at DATETIME NOT NULL,
                    PRIMARY KEY (id)
                )
                """
            )
        )
        conn.execute(
            text(
                """
                CREATE TABLE task_center_runs (
                    id VARCHAR(36) NOT NULL,
                    request_id VARCHAR(36) NOT NULL,
                    root_task_id VARCHAR(96),
                    status VARCHAR(32) NOT NULL,
                    started_at DATETIME NOT NULL,
                    finished_at DATETIME,
                    PRIMARY KEY (id)
                )
                """
            )
        )
        conn.execute(
            text(
                """
                CREATE TABLE task_center_tasks (
                    id VARCHAR(96) NOT NULL,
                    run_id VARCHAR(36) NOT NULL,
                    role VARCHAR(32) NOT NULL,
                    rendered_prompt TEXT NOT NULL,
                    status VARCHAR(32) NOT NULL,
                    summaries JSON NOT NULL,
                    needs JSON NOT NULL,
                    task_center_attempt_id VARCHAR(96),
                    fix_target_id VARCHAR(96),
                    spawn_reason VARCHAR(64),
                    created_at DATETIME NOT NULL,
                    updated_at DATETIME NOT NULL,
                    PRIMARY KEY (id)
                )
                """
            )
        )
        conn.execute(
            text(
                """
                CREATE TABLE task_center_attempt (
                    id VARCHAR(96) NOT NULL,
                    run_id VARCHAR(36) NOT NULL,
                    root_task_id VARCHAR(96) NOT NULL,
                    planner_task_id VARCHAR(96) NOT NULL,
                    executor_task_ids JSON NOT NULL,
                    dag_nodes JSON NOT NULL,
                    -- Legacy Phase 05 migration fixture: historical schemas may include plan_shape.
                    plan_shape VARCHAR(16),
                    what_to_do_next TEXT NOT NULL,
                    prior_graph_id VARCHAR(96),
                    created_at DATETIME NOT NULL,
                    updated_at DATETIME NOT NULL,
                    PRIMARY KEY (id)
                )
                """
            )
        )
        conn.execute(
            text(
                """
                INSERT INTO task_center_requests (
                    id, cwd, sandbox_id, request_prompt, created_at, updated_at
                )
                VALUES (
                    'req', '/repo', NULL, 'prompt',
                    '2026-01-01 00:00:00', '2026-01-01 00:00:00'
                )
                """
            )
        )
        conn.execute(
            text(
                """
                INSERT INTO task_center_runs (
                    id, request_id, root_task_id, status, started_at, finished_at
                )
                VALUES (
                    'tc-run', 'req', NULL, 'running',
                    '2026-01-01 00:00:00', NULL
                )
                """
            )
        )
        conn.execute(
            text(
                """
                INSERT INTO task_center_tasks (
                    id, run_id, role, rendered_prompt, status, summaries, needs,
                    task_center_attempt_id, fix_target_id, spawn_reason,
                    created_at, updated_at
                )
                VALUES (
                    'tc-run:t1', 'tc-run', 'executor', 'prompt', 'running',
                    '[]', '[]', NULL, NULL, NULL,
                    '2026-01-01 00:00:00', '2026-01-01 00:00:00'
                )
                """
            )
        )
        conn.execute(
            text(
                """
                INSERT INTO task_center_attempt (
                    id, run_id, root_task_id, planner_task_id, executor_task_ids,
                    dag_nodes, plan_shape, what_to_do_next, prior_graph_id,
                    created_at, updated_at
                )
                VALUES (
                    'graph-1', 'tc-run', 'tc-run:t1', 'planner-1', '[]',
                    '[]', NULL, '', NULL,
                    '2026-01-01 00:00:00', '2026-01-01 00:00:00'
                )
                """
            )
        )
    legacy_engine.dispose()

    sf = engine_mod.initialize_db(DatabaseSettings(url=f"sqlite:///{db_path}"))
    assert sf is not None

    engine = engine_mod.get_engine()
    assert engine is not None
    insp = inspect(engine)
    columns = {col["name"] for col in insp.get_columns("task_center_tasks")}
    run_columns = {col["name"] for col in insp.get_columns("task_center_runs")}
    assert "task_center_run_id" in columns
    assert "agent_name" in columns
    assert "run_id" not in columns
    assert "root_task_id" not in run_columns
    # Phase 01 drops the legacy task_center_attempt table after init.
    assert not insp.has_table("task_center_attempt")
    assert insp.has_table("attempts")
    with engine.connect() as conn:
        task_run_id = conn.execute(
            text('SELECT task_center_run_id FROM task_center_tasks WHERE id = :id'),
            {"id": "tc-run:t1"},
        ).scalar_one()
    assert task_run_id == "tc-run"


def test_initialize_db_migrates_legacy_agent_runs_schema(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
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
    task_center_store.create_run(task_center_run_id="run", request_id="req")
    task_center_store.upsert_task(
        task_id="run:t1",
        task_center_run_id="run",
        role="executor",
        agent_name="executor",
        rendered_prompt="prompt",
        status="running",
        summaries=[],
        needs=[],
        task_center_attempt_id=None,
    )

    agent_run_store.create_run(
        agent_run_id="agent1",
        task_id="run:t1",
        agent_name="executor",
    )

    tasks = task_center_store.list_tasks_for_run("run")
    assert tasks[0]["task_center_run_id"] == "run"
    assert "run_id" not in tasks[0]
    assert agent_run_store.get_run("agent1") is not None


def test_initialize_db_drops_dead_task_center_lifecycle_columns(
    tmp_path: Path,
) -> None:
    db_path = tmp_path / "dead-columns.db"
    legacy_engine = create_engine(f"sqlite:///{db_path}")
    Base.metadata.create_all(legacy_engine)
    with legacy_engine.begin() as conn:
        conn.execute(text('ALTER TABLE "task_center_tasks" ADD COLUMN "system_prompt" TEXT'))
        conn.execute(text('ALTER TABLE "task_center_tasks" ADD COLUMN "user_prompt" TEXT'))
    legacy_engine.dispose()

    sf = engine_mod.initialize_db(DatabaseSettings(url=f"sqlite:///{db_path}"))
    assert sf is not None

    engine = engine_mod.get_engine()
    assert engine is not None
    insp = inspect(engine)
    task_columns = {col["name"] for col in insp.get_columns("task_center_tasks")}
    assert "system_prompt" not in task_columns
    assert "user_prompt" not in task_columns
