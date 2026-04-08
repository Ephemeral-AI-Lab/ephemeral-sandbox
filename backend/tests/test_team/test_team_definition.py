"""Unit tests for ``TeamDefinition``, ``TeamDefinitionStore``, and the
``TeamRun.start_with_team_definition`` dispatch path."""

from __future__ import annotations

import asyncio

import pytest
from sqlalchemy import create_engine
from sqlalchemy.orm import sessionmaker

from db.base import Base
# Importing the model registers the table on ``Base.metadata`` so
# ``create_all`` picks it up below.
from team.db.model import TeamDefinitionRecord  # noqa: F401
from team.db.store import TeamDefinitionStore
from team.run import TeamRun
from team.types import TeamDefinition, TeamRunStatus


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------


@pytest.fixture
def session_factory():
    engine = create_engine("sqlite:///:memory:", echo=False)
    Base.metadata.create_all(engine)
    return sessionmaker(bind=engine, autoflush=False, expire_on_commit=False)


@pytest.fixture
def store(session_factory) -> TeamDefinitionStore:
    s = TeamDefinitionStore()
    s.initialize(session_factory)
    return s


# ---------------------------------------------------------------------------
# TeamDefinitionStore CRUD
# ---------------------------------------------------------------------------


def test_create_and_get_by_name(store: TeamDefinitionStore) -> None:
    td = store.create(
        name="default",
        planner_agent="team_planner",
        worker_agents=["team_worker"],
        description="default team",
    )
    assert td.name == "default"
    assert td.planner_agent == "team_planner"
    assert td.worker_agents == ["team_worker"]
    assert td.description == "default team"
    assert td.id  # uuid assigned

    fetched = store.get_by_name("default")
    assert fetched is not None
    assert fetched.id == td.id
    assert fetched.planner_agent == "team_planner"


def test_create_rejects_duplicate_name(store: TeamDefinitionStore) -> None:
    store.create(name="dup", planner_agent="planner_a")
    with pytest.raises(ValueError, match="already exists"):
        store.create(name="dup", planner_agent="planner_b")


def test_get_by_name_missing_returns_none(store: TeamDefinitionStore) -> None:
    assert store.get_by_name("nonexistent") is None


def test_list_all_sorted_by_name(store: TeamDefinitionStore) -> None:
    store.create(name="zebra", planner_agent="p")
    store.create(name="alpha", planner_agent="p")
    store.create(name="mike", planner_agent="p")
    names = [td.name for td in store.list_all()]
    assert names == ["alpha", "mike", "zebra"]


def test_delete_removes_row(store: TeamDefinitionStore) -> None:
    store.create(name="x", planner_agent="p")
    assert store.delete("x") is True
    assert store.get_by_name("x") is None
    # Idempotent on missing row.
    assert store.delete("x") is False


def test_get_or_create_is_idempotent(store: TeamDefinitionStore) -> None:
    first = store.get_or_create(
        name="default",
        planner_agent="team_planner",
        worker_agents=["team_worker"],
    )
    second = store.get_or_create(
        name="default",
        planner_agent="DIFFERENT",  # should NOT overwrite an existing row
        worker_agents=["DIFFERENT"],
    )
    assert first.id == second.id
    assert second.planner_agent == "team_planner"
    assert second.worker_agents == ["team_worker"]


def test_worker_agents_defaults_to_empty_list(store: TeamDefinitionStore) -> None:
    td = store.create(name="bare", planner_agent="p")
    assert td.worker_agents == []
    fetched = store.get_by_name("bare")
    assert fetched is not None
    assert fetched.worker_agents == []


# ---------------------------------------------------------------------------
# TeamRun.start_with_team_definition
# ---------------------------------------------------------------------------


class _NoopWorker:
    """A ``Worker`` stand-in whose ``run_forever`` exits immediately.

    Lets us drive ``TeamRun.start(...)`` without spinning up the real
    engine. Since no worker ever pops the ready queue, we rely on
    ``TeamRun.cancel`` to drive every WorkItem to a terminal state.
    """

    def __init__(self, team_run: TeamRun) -> None:
        self.team_run = team_run

    async def run_forever(self) -> None:
        return None


def _noop_worker_factory(team_run: TeamRun) -> _NoopWorker:
    return _NoopWorker(team_run)


def _stub_registry(known: set[str], monkeypatch: pytest.MonkeyPatch) -> None:
    """Patch ``agents.registry.get_definition`` to return a truthy
    placeholder for names in *known* and ``None`` for everything else.

    ``TeamRun.start_with_team_definition`` imports ``get_definition``
    lazily inside the method, so patching the module attribute BEFORE the
    call is sufficient — the lookup happens at method entry.
    """
    from agents import registry

    sentinel = object()  # any truthy placeholder; only ``is None`` is checked.

    def _fake(name: str):
        return sentinel if name in known else None

    monkeypatch.setattr(registry, "get_definition", _fake)


async def _cleanup_run(run: TeamRun) -> None:
    """Drive a successfully-started ``TeamRun`` to a terminal state."""
    if run.root_work_item_id is None:
        return  # start() never happened; nothing to clean up
    await run.cancel()
    try:
        await asyncio.wait_for(run.wait(), timeout=2.0)
    except asyncio.TimeoutError:
        pass


@pytest.mark.asyncio
async def test_start_with_team_definition_spawns_root_with_planner(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    _stub_registry({"my_planner"}, monkeypatch)
    team_def = TeamDefinition(
        id="tdef-1",
        name="default",
        description="",
        planner_agent="my_planner",
        worker_agents=["my_worker"],
    )
    run = TeamRun(session_id="s", user_request="do stuff")
    try:
        await run.start_with_team_definition(
            team_def,
            payload={"k": "v"},
            worker_factory=_noop_worker_factory,
        )
        assert run.status == TeamRunStatus.RUNNING
        assert run.root_work_item_id is not None
        root = run.dispatcher.graph[run.root_work_item_id]
        assert root.agent_name == "my_planner"
        assert root.payload == {"k": "v"}
    finally:
        await _cleanup_run(run)


@pytest.mark.asyncio
async def test_start_with_team_definition_rejects_unknown_planner(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    _stub_registry(set(), monkeypatch)  # no known agents
    team_def = TeamDefinition(
        id="tdef-2",
        name="broken",
        description="",
        planner_agent="ghost",
    )
    run = TeamRun(session_id="s", user_request="do stuff")
    with pytest.raises(ValueError, match="ghost"):
        await run.start_with_team_definition(
            team_def,
            payload={},
            worker_factory=_noop_worker_factory,
        )
    # No root WorkItem should have been inserted; TeamRun stays PENDING.
    assert run.root_work_item_id is None
    assert run.status == TeamRunStatus.PENDING
    assert len(run.dispatcher.graph) == 0


@pytest.mark.asyncio
async def test_start_with_team_definition_error_message_names_team_and_agent(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    _stub_registry(set(), monkeypatch)
    team_def = TeamDefinition(
        id="tdef-3",
        name="frontend_team",
        description="",
        planner_agent="missing_planner",
    )
    run = TeamRun(session_id="s", user_request="do stuff")
    with pytest.raises(ValueError) as exc_info:
        await run.start_with_team_definition(
            team_def,
            payload={},
            worker_factory=_noop_worker_factory,
        )
    msg = str(exc_info.value)
    assert "frontend_team" in msg
    assert "missing_planner" in msg
