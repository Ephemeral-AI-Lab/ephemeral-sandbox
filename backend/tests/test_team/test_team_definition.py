"""Unit tests for ``TeamDefinition`` and ``TeamRun.start_with_team_definition``."""

from __future__ import annotations

import asyncio

import pytest

from team.core.models import TaskStatus, TaskStatusUpdate, TeamDefinition, TeamRunStatus
from team.persistence.run_store import TeamRunStore
from team.runtime.services import TeamRuntimeServices
from team.runtime.team_run import TeamRun


def _spec(goal: str) -> dict[str, str]:
    return {
        "goal": goal,
        "detail": f"Detail for {goal}",
        "acceptance_criteria": f"Acceptance for {goal}",
    }


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
        from team.core.models import BudgetConfig, BudgetState

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
    from team.core.models import ProjectContext

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
            payload={"spec": _spec("Plan the requested work"), "scope_paths": ["src/app.py"]},
            executor_factory=_noop_executor_factory,
        )
        assert run.status == TeamRunStatus.RUNNING
        assert run.team_definition == team_def
        assert run.root_task_id is not None
        root = run.task_center.graph[run.root_task_id]
        assert root.agent_name == "my_planner"
        assert root.definition.spec.goal == "Plan the requested work"
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
    with pytest.raises(ValueError, match="Root payload requires a non-empty 'spec'"):
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
