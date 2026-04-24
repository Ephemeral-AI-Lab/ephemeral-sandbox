"""Unit tests for ``TeamDefinition``, ``TeamDefinitionStore``, and the
``TeamRun.start_with_team_definition`` dispatch path."""

from __future__ import annotations

import asyncio

import pytest
from sqlalchemy import create_engine
from sqlalchemy.orm import sessionmaker

# Importing the model registers the table on ``Base.metadata`` so
# ``create_all`` picks it up below.
from team.models import TaskStatus, TaskStatusUpdate, TeamDefinition, TeamRunStatus
from team.persistence.model import TeamDefinitionRecord  # noqa: F401
from team.persistence.run_store import TeamRunStore
from team.persistence.store import TeamDefinitionStore
from team.runtime.services import TeamRuntimeServices
from team.runtime.team_run import TeamRun


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------


@pytest.fixture
def session_factory():
    engine = create_engine("sqlite:///:memory:", echo=False)
    # Only create tables this test needs (ARRAY columns in TaskRecord
    # are incompatible with SQLite).
    TeamDefinitionRecord.__table__.create(engine, checkfirst=True)
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
        entry_planner="team_planner",
        roster={
            "planner": ["team_planner"],
            "developer": ["developer"],
            "reviewer": ["validator"],
        },
        description="default team",
    )
    assert td.name == "default"
    assert td.entry_planner == "team_planner"
    assert td.roster["developer"] == ["developer"]
    assert td.description == "default team"
    assert td.id  # uuid assigned

    fetched = store.get_by_name("default")
    assert fetched is not None
    assert fetched.id == td.id
    assert fetched.entry_planner == "team_planner"
    assert fetched.roster["planner"] == ["team_planner"]


def test_create_populates_current_schema_columns(store: TeamDefinitionStore) -> None:
    store.create(
        name="dual-write",
        entry_planner="team_planner",
        roster={
            "planner": ["team_planner"],
            "developer": ["developer"],
            "reviewer": ["validator"],
            "explorer": ["scout"],
        },
    )

    with store._sf() as db:  # noqa: SLF001
        record = (
            db.query(TeamDefinitionRecord)
            .filter(TeamDefinitionRecord.name == "dual-write")
            .one()
        )

    assert record.entry_planner == "team_planner"
    assert record.planner_agent == "team_planner"
    assert record.worker_agents == ["developer", "validator", "scout"]


def test_get_by_name_falls_back_to_current_schema_columns(store: TeamDefinitionStore) -> None:
    with store._sf() as db:  # noqa: SLF001
        db.add(
            TeamDefinitionRecord(
                id="current-schema-row",
                name="current-schema",
                description="current only",
                planner_agent="team_planner",
                worker_agents=["developer", "validator"],
                roster=None,
                entry_planner=None,
            )
        )
        db.commit()

    fetched = store.get_by_name("current-schema")

    assert fetched is not None
    assert fetched.entry_planner == "team_planner"
    assert fetched.roster == {
        "planner": ["team_planner"],
        "worker": ["developer", "validator"],
    }


def test_create_rejects_duplicate_name(store: TeamDefinitionStore) -> None:
    store.create(name="dup", entry_planner="p", roster={"planner": ["p"]})
    with pytest.raises(ValueError, match="already exists"):
        store.create(name="dup", entry_planner="p2", roster={"planner": ["p2"]})


def test_get_by_name_missing_returns_none(store: TeamDefinitionStore) -> None:
    assert store.get_by_name("nonexistent") is None


def test_list_all_sorted_by_name(store: TeamDefinitionStore) -> None:
    store.create(name="zebra", entry_planner="p", roster={"planner": ["p"]})
    store.create(name="alpha", entry_planner="p", roster={"planner": ["p"]})
    store.create(name="mike", entry_planner="p", roster={"planner": ["p"]})
    names = [td.name for td in store.list_all()]
    assert names == ["alpha", "mike", "zebra"]


def test_delete_removes_row(store: TeamDefinitionStore) -> None:
    store.create(name="x", entry_planner="p", roster={"planner": ["p"]})
    assert store.delete("x") is True
    assert store.get_by_name("x") is None
    # Idempotent on missing row.
    assert store.delete("x") is False


def test_roster_with_multiple_agents_per_role(store: TeamDefinitionStore) -> None:
    td = store.create(
        name="multi",
        entry_planner="team_planner",
        roster={
            "planner": ["team_planner"],
            "developer": ["dev_python", "dev_rust", "dev_go"],
            "reviewer": ["unit_tester", "integration_tester"],
            "explorer": ["scout"],
        },
    )
    assert td.roster["developer"] == ["dev_python", "dev_rust", "dev_go"]
    assert td.roster["reviewer"] == ["unit_tester", "integration_tester"]

    fetched = store.get_by_name("multi")
    assert fetched is not None
    assert fetched.roster["developer"] == ["dev_python", "dev_rust", "dev_go"]


def test_roster_defaults_to_empty_dict(store: TeamDefinitionStore) -> None:
    td = store.create(name="bare", entry_planner="p", roster={})
    assert td.roster == {}
    fetched = store.get_by_name("bare")
    assert fetched is not None
    assert fetched.roster == {}


def test_seed_builtin_populates_current_schema_columns(store: TeamDefinitionStore) -> None:
    td = store.seed_builtin(
        TeamDefinition(
            id="builtin-1",
            name="builtin",
            description="builtin team",
            entry_planner="team_planner",
            roster={
                "planner": ["team_planner"],
                "developer": ["developer"],
                "reviewer": ["validator"],
            },
        )
    )

    assert td.entry_planner == "team_planner"

    with store._sf() as db:  # noqa: SLF001
        record = (
            db.query(TeamDefinitionRecord)
            .filter(TeamDefinitionRecord.name == "builtin")
            .one()
        )

    assert record.planner_agent == "team_planner"
    assert record.worker_agents == ["developer", "validator"]



# ---------------------------------------------------------------------------
# TeamRun.start_with_team_definition
# ---------------------------------------------------------------------------


class _NoopWorker:
    def __init__(self, team_run: TeamRun) -> None:
        self.team_run = team_run

    async def run(self, task_id: str) -> TaskStatusUpdate:
        return TaskStatusUpdate(task_id=task_id, status=TaskStatus.CANCELLED, summary="noop")

    async def post_dispatch(self, update: TaskStatusUpdate) -> None:
        return None


def _noop_executor_factory(team_run: TeamRun) -> _NoopWorker:
    return _NoopWorker(team_run)


class _FakeStore:
    """Sync TaskStore surface (matches production ``TaskStore`` API)."""

    def __init__(self, graph: dict) -> None:
        self.graph = graph

    def get_task(self, task_id: str):
        return self.graph.get(task_id)

    async def get_record(self, task_id: str):
        return self.graph.get(task_id)

    async def get_statuses(self) -> dict[str, str]:
        return {"task-1": "cancelled"}

    async def all_terminal(self) -> bool:
        return True

    async def cancel_all_pending(self) -> int:
        for task in self.graph.values():
            task.status = TaskStatus.CANCELLED
        return len(self.graph)

    async def cancel_all_running(self, reason: str) -> int:
        return 0


class _FakeTaskCenter:
    def __init__(self) -> None:
        from team.models import BudgetConfig, BudgetState

        self.budgets = BudgetConfig()
        self.budget_state = BudgetState()
        self.graph = {}
        self._events = TeamRunStore()
        self.notes = []
        self.budget = None
        self.expander = None
        self.store = _FakeStore(self.graph)

    def emit_event(self, event) -> None:
        pass

    async def add_task(self, task) -> None:
        self.budget_state.tasks_used += 1
        self.graph[task.id] = task

    async def get_task(self, task_id: str):
        return self.graph.get(task_id)

def _fake_services() -> TeamRuntimeServices:
    from team.context.project import ProjectContext

    tc = _FakeTaskCenter()
    return TeamRuntimeServices(
        project_context=ProjectContext(goal="", user_request="", project_key="", repo_root=""),
        task_center=tc,  # type: ignore[arg-type]
        event_store=TeamRunStore(),
    )


def _stub_registry(known: set[str], monkeypatch: pytest.MonkeyPatch) -> None:
    from types import SimpleNamespace
    from agents import registry

    def _fake(name: str):
        if name not in known:
            return None
        return SimpleNamespace(role="planner", name=name)

    monkeypatch.setattr(registry, "get_definition", _fake)


async def _cleanup_run(run: TeamRun) -> None:
    if run.root_task_id is None:
        return
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
        entry_planner="my_planner",
        roster={"planner": ["my_planner"]},
    )
    run = TeamRun(session_id="s", user_request="do stuff", services=_fake_services())
    try:
        await run.start_with_team_definition(
            team_def,
            payload={"objective": "Plan the requested work", "scope_paths": ["src/app.py"]},
            executor_factory=_noop_executor_factory,
        )
        assert run.status == TeamRunStatus.RUNNING
        assert run.team_definition == team_def
        assert run.root_task_id is not None
        root = run.task_center.graph[run.root_task_id]
        assert root.agent_name == "my_planner"
        assert root.objective == "Plan the requested work"
        assert root.scope_paths == ["src/app.py"]
    finally:
        await _cleanup_run(run)


@pytest.mark.asyncio
async def test_start_with_team_definition_rejects_legacy_task_payload(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    _stub_registry({"my_planner"}, monkeypatch)
    team_def = TeamDefinition(
        id="tdef-legacy",
        name="default",
        description="",
        entry_planner="my_planner",
        roster={"planner": ["my_planner"]},
    )
    run = TeamRun(session_id="s", user_request="do stuff", services=_fake_services())
    with pytest.raises(ValueError, match="Root payload requires a non-empty 'objective'"):
        await run.start_with_team_definition(
            team_def,
            payload={"task": "legacy prompt"},
            executor_factory=_noop_executor_factory,
        )
    assert run.root_task_id is None
    assert len(run.task_center.graph) == 0


@pytest.mark.asyncio
async def test_start_with_team_definition_rejects_unknown_planner(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    _stub_registry(set(), monkeypatch)
    team_def = TeamDefinition(
        id="tdef-2",
        name="broken",
        description="",
        entry_planner="ghost",
        roster={"planner": ["ghost"]},
    )
    run = TeamRun(session_id="s", user_request="do stuff", services=_fake_services())
    with pytest.raises(ValueError, match="ghost"):
        await run.start_with_team_definition(
            team_def,
            payload={},
            executor_factory=_noop_executor_factory,
        )
    assert run.root_task_id is None
    assert run.status == TeamRunStatus.PENDING
    assert len(run.task_center.graph) == 0


@pytest.mark.asyncio
async def test_start_with_team_definition_error_message_names_team_and_agent(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    _stub_registry(set(), monkeypatch)
    team_def = TeamDefinition(
        id="tdef-3",
        name="frontend_team",
        description="",
        entry_planner="missing_planner",
        roster={"planner": ["missing_planner"]},
    )
    run = TeamRun(session_id="s", user_request="do stuff", services=_fake_services())
    with pytest.raises(ValueError) as exc_info:
        await run.start_with_team_definition(
            team_def,
            payload={},
            executor_factory=_noop_executor_factory,
        )
    msg = str(exc_info.value)
    assert "frontend_team" in msg
    assert "missing_planner" in msg
