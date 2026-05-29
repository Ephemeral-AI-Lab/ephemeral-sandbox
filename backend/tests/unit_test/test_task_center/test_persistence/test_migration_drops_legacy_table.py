"""Migration test: legacy task_center_attempt table is dropped."""

from __future__ import annotations

from sqlalchemy import create_engine, inspect, text

import db.engine as engine_mod
from config.settings import DatabaseSettings
from db.stores.workflow_store import WorkflowStore


def test_initialize_db_drops_legacy_attempt_table(tmp_path, monkeypatch):
    db_path = tmp_path / "legacy.db"

    # Pre-seed the legacy table with a row to confirm it gets dropped.
    pre_engine = create_engine(f"sqlite:///{db_path}")
    with pre_engine.begin() as conn:
        conn.execute(
            text(
                "CREATE TABLE task_center_attempt "
                "(id TEXT PRIMARY KEY, run_id TEXT)"
            )
        )
        conn.execute(
            text(
                "INSERT INTO task_center_attempt (id, run_id) "
                "VALUES ('legacy-1', 'r1')"
            )
        )
    pre_engine.dispose()

    # Reset module-level engine state so initialize_db rebuilds cleanly.
    monkeypatch.setattr(engine_mod, "_engine", None)
    monkeypatch.setattr(engine_mod, "_session_factory", None)

    sf = engine_mod.initialize_db(DatabaseSettings(url=f"sqlite:///{db_path}"))
    assert sf is not None
    eng = engine_mod.get_engine()
    assert eng is not None
    insp = inspect(eng)
    tables = set(insp.get_table_names())
    assert "task_center_attempt" not in tables
    assert "workflows" in tables
    assert "iterations" in tables
    assert "attempts" in tables


def test_initialize_db_creates_workflows_table_on_fresh_db(tmp_path, monkeypatch):
    db_path = tmp_path / "fresh.db"
    _reset_engine(monkeypatch)

    sf = engine_mod.initialize_db(DatabaseSettings(url=f"sqlite:///{db_path}"))
    assert sf is not None

    eng = engine_mod.get_engine()
    assert eng is not None
    tables = set(inspect(eng).get_table_names())
    assert "workflows" in tables
    assert "goals" not in tables

    store = WorkflowStore()
    store.initialize(sf)
    workflow = store.insert(task_center_run_id="run1", goal="fresh objective")
    assert store.get(workflow.id) == workflow


def test_initialize_db_renames_legacy_goals_table_and_iteration_column(
    tmp_path, monkeypatch
):
    db_path = tmp_path / "legacy-workflow.db"
    pre_engine = create_engine(f"sqlite:///{db_path}")
    with pre_engine.begin() as conn:
        conn.execute(
            text(
                """
                CREATE TABLE goals (
                    id TEXT PRIMARY KEY,
                    task_center_run_id TEXT NOT NULL,
                    origin_kind TEXT,
                    requested_by_task_id TEXT,
                    goal TEXT NOT NULL,
                    status TEXT NOT NULL,
                    iteration_ids JSON,
                    final_outcome JSON,
                    created_at DATETIME NOT NULL,
                    updated_at DATETIME NOT NULL,
                    closed_at DATETIME
                )
                """
            )
        )
        conn.execute(
            text(
                """
                INSERT INTO goals (
                    id, task_center_run_id, origin_kind, requested_by_task_id,
                    goal, status, iteration_ids, final_outcome, created_at, updated_at
                )
                VALUES (
                    'wf1', 'run1', 'entry', NULL, 'legacy objective', 'open',
                    '[]', NULL, '2026-01-01 00:00:00', '2026-01-01 00:00:00'
                )
                """
            )
        )
        conn.execute(
            text(
                """
                CREATE TABLE iterations (
                    id TEXT PRIMARY KEY,
                    goal_id TEXT NOT NULL,
                    sequence_no INTEGER NOT NULL,
                    creation_reason TEXT NOT NULL,
                    goal TEXT NOT NULL,
                    attempt_budget INTEGER NOT NULL,
                    status TEXT NOT NULL,
                    attempt_ids JSON,
                    deferred_goal TEXT,
                    created_at DATETIME NOT NULL,
                    updated_at DATETIME NOT NULL,
                    closed_at DATETIME,
                    task_specification TEXT,
                    task_summary TEXT
                )
                """
            )
        )
        conn.execute(
            text(
                """
                INSERT INTO iterations (
                    id, goal_id, sequence_no, creation_reason, goal, attempt_budget,
                    status, attempt_ids, created_at, updated_at
                )
                VALUES (
                    'it1', 'wf1', 1, 'initial', 'legacy objective', 3,
                    'open', '[]', '2026-01-01 00:00:00', '2026-01-01 00:00:00'
                )
                """
            )
        )
    pre_engine.dispose()

    _reset_engine(monkeypatch)
    sf = engine_mod.initialize_db(DatabaseSettings(url=f"sqlite:///{db_path}"))
    assert sf is not None

    eng = engine_mod.get_engine()
    assert eng is not None
    insp = inspect(eng)
    assert "goals" not in set(insp.get_table_names())
    assert "workflows" in set(insp.get_table_names())
    iteration_columns = {column["name"] for column in insp.get_columns("iterations")}
    assert "workflow_id" in iteration_columns
    assert "goal_id" not in iteration_columns

    store = WorkflowStore()
    store.initialize(sf)
    workflow = store.get("wf1")
    assert workflow is not None
    assert workflow.goal == "legacy objective"


def _reset_engine(monkeypatch) -> None:
    monkeypatch.setattr(engine_mod, "_engine", None)
    monkeypatch.setattr(engine_mod, "_session_factory", None)
