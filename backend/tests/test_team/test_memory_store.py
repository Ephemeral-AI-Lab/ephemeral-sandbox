"""Tests for durable typed team memory persistence."""

from __future__ import annotations

from sqlalchemy import create_engine
from sqlalchemy.orm import sessionmaker

from team.memory.model import TeamMemoryRecordModel  # noqa: F401
from team.memory.runtime import persist_memory_record
from team.memory.store import TeamMemoryRecord, TeamMemoryStore


def _memory_store() -> TeamMemoryStore:
    engine = create_engine("sqlite:///:memory:", echo=False)
    # Only create the tables this test needs (ARRAY columns in other
    # models are incompatible with SQLite).
    TeamMemoryRecordModel.__table__.create(engine, checkfirst=True)
    factory = sessionmaker(bind=engine, autoflush=False, expire_on_commit=False)
    store = TeamMemoryStore()
    store.initialize(factory)
    return store


def test_team_memory_store_roundtrip_and_query(monkeypatch) -> None:
    store = _memory_store()
    monkeypatch.setattr("team.memory.runtime.get_default_store", lambda: store)

    persisted = persist_memory_record(
        project_key="P1",
        repo_root="/repo",
        kind="architecture_decision",
        scope={"paths": ["src/runtime"]},
        content={"decision": "publish from worker directly"},
        source={"team_run_id": "T1", "agent": "planner"},
    )

    assert persisted is True
    results = store.query(
        project_key="P1",
        kinds=["architecture_decision"],
        scope_paths=["src/runtime"],
    )
    assert len(results) == 1
    assert results[0].content["decision"] == "publish from worker directly"


def test_team_memory_query_applies_scope_filter_before_limit() -> None:
    store = _memory_store()
    store.append_many(
        [
            TeamMemoryRecord(
                project_key="P1",
                repo_root="/repo",
                kind="conflict_event",
                scope={"paths": ["src/other.py"]},
                content={"n": idx},
                observed_at=100.0 - idx,
            )
            for idx in range(5)
        ]
    )
    store.append(
        TeamMemoryRecord(
            project_key="P1",
            repo_root="/repo",
            kind="conflict_event",
            scope={"paths": ["src/target.py"]},
            content={"n": 999},
            observed_at=1.0,
        )
    )

    results = store.query(
        project_key="P1",
        kinds=["conflict_event"],
        scope_paths=["src/target.py"],
        limit=3,
    )

    assert len(results) == 1
    assert results[0].content["n"] == 999
