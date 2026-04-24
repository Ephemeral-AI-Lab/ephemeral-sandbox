"""Unit tests for ``Executor.run`` and ``translate_tool_metadata``."""

from __future__ import annotations

import asyncio
from datetime import datetime, timezone
from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest

from team.models import (
    Plan,
    ReplanPlan,
    Task,
    TaskStatus,
    TaskStatusUpdate,
)
from team.runtime.context_builder import TeamAgentContext
from team.runtime.executor import Executor, translate_tool_metadata


# ---------------------------------------------------------------------------
# Minimal fakes
# ---------------------------------------------------------------------------


class FakeDefn:
    role = "developer"
    name = "developer"


class _NotesProxy(list):
    async def post(self, note):
        self.append(note)


class FakeStore:
    def __init__(self, task):
        self._task = task
        self.mark_running = AsyncMock(return_value=task)

    def get_task(self, task_id):
        return self._task if self._task and self._task.id == task_id else None


class FakeTaskCenter:
    def __init__(self, task=None):
        self.notes = _NotesProxy()
        self.activity = self
        self.store = FakeStore(task)
        self.emitted: list = []

    def emit_event(self, event):
        self.emitted.append(event)


class FakeTeamRun:
    def __init__(self, task_center=None, arbiter=None):
        self.id = "test-run-001"
        self.task_center = task_center or FakeTaskCenter()
        self.arbiter = arbiter
        self._active_agent_runs: dict[str, asyncio.Task[object]] = {}
        self.cancel_event = asyncio.Event()

    def register_agent_run(self, task_id, runner_task):
        self._active_agent_runs[task_id] = runner_task

    def unregister_agent_run(self, task_id, runner_task):
        if self._active_agent_runs.get(task_id) is runner_task:
            self._active_agent_runs.pop(task_id, None)


def _make_task(
    *,
    status: str = "running",
    parent_id: str | None = "parent-1",
    agent_run_id: str = "agent-run-1",
) -> Task:
    return Task(
        id="task-1",
        team_run_id="test-run-001",
        agent_name="developer",
        status=TaskStatus(status),
        objective="fix the bug",
        scope_paths=["src/auth/"],
        parent_id=parent_id,
        root_id="root-1",
        agent_run_id=agent_run_id,
        created_at=datetime.now(timezone.utc),
    )


# ---------------------------------------------------------------------------
# translate_tool_metadata
# ---------------------------------------------------------------------------


def test_translate_success():
    ctx = TeamAgentContext(
        tool_metadata={"task_summary_type": "success", "task_summary": "Fixed auth"}
    )
    update = translate_tool_metadata("t1", ctx)
    assert update.status is TaskStatus.DONE
    assert update.summary == "Fixed auth"
    assert update.plan is None and update.replan is None


def test_translate_request_replan():
    ctx = TeamAgentContext(
        tool_metadata={"task_summary_type": "request_replan", "task_summary": "needs split"}
    )
    update = translate_tool_metadata("t1", ctx)
    assert update.status is TaskStatus.REQUEST_REPLAN
    assert update.summary == "needs split"


def test_translate_plan():
    plan = Plan.from_dict({"tasks": [{"id": "a", "objective": "o", "agent": "developer"}]})
    ctx = TeamAgentContext(tool_metadata={"resolved_plan": plan, "plan_is_replan": False})
    update = translate_tool_metadata("t1", ctx)
    assert update.status is TaskStatus.EXPANDED
    assert update.plan is plan
    assert update.replan is None


def test_translate_replan():
    replan = ReplanPlan.from_dict(
        {"add_tasks": [{"id": "a", "objective": "o", "agent": "developer"}], "cancel_ids": ["x"]}
    )
    ctx = TeamAgentContext(tool_metadata={"resolved_plan": replan, "plan_is_replan": True})
    update = translate_tool_metadata("t1", ctx)
    assert update.status is TaskStatus.EXPANDED
    assert update.replan is replan
    assert update.plan is None


def test_translate_no_terminal_tool_is_failed():
    ctx = TeamAgentContext(tool_metadata={"work_result": "still thinking..."})
    update = translate_tool_metadata("t1", ctx)
    assert update.status is TaskStatus.FAILED
    assert update.summary.startswith("no_terminal_tool_called")
    assert "still thinking" in update.summary


def test_translate_empty_metadata_is_failed():
    update = translate_tool_metadata("t1", TeamAgentContext(tool_metadata={}))
    assert update.status is TaskStatus.FAILED
    assert update.summary == "no_terminal_tool_called"


# ---------------------------------------------------------------------------
# Executor.run returns a TaskStatusUpdate; never touches a handler itself
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_run_translates_runner_success_to_done_update():
    task = _make_task()
    team_run = FakeTeamRun(task_center=FakeTaskCenter(task=task))

    async def runner(_defn, ctx: TeamAgentContext) -> None:
        ctx.tool_metadata["task_summary_type"] = "success"
        ctx.tool_metadata["task_summary"] = "done"

    executor = Executor(
        team_run=team_run,
        runner=runner,
        agent_lookup=lambda name: FakeDefn(),
        build_query_context=AsyncMock(return_value=TeamAgentContext(user_message="ctx")),
    )

    update = await executor.run(task.id)

    assert update.status is TaskStatus.DONE
    assert update.summary == "done"


@pytest.mark.asyncio
async def test_run_unknown_agent_returns_failed():
    task = _make_task()
    team_run = FakeTeamRun(task_center=FakeTaskCenter(task=task))

    executor = Executor(
        team_run=team_run,
        runner=AsyncMock(),
        agent_lookup=lambda name: None,
        build_query_context=AsyncMock(return_value=TeamAgentContext(user_message="ctx")),
    )

    update = await executor.run(task.id)

    assert update.status is TaskStatus.FAILED
    assert "unknown_agent" in update.summary


@pytest.mark.asyncio
async def test_run_runner_exception_returns_failed():
    task = _make_task()
    team_run = FakeTeamRun(task_center=FakeTaskCenter(task=task))

    async def runner(_defn, _ctx):
        raise RuntimeError("boom")

    executor = Executor(
        team_run=team_run,
        runner=runner,
        agent_lookup=lambda name: FakeDefn(),
        build_query_context=AsyncMock(return_value=TeamAgentContext(user_message="ctx")),
    )

    update = await executor.run(task.id)

    assert update.status is TaskStatus.FAILED
    assert "runner_exception" in update.summary and "boom" in update.summary


@pytest.mark.asyncio
async def test_run_cooperative_cancel_returns_cancelled():
    task = _make_task()
    team_run = FakeTeamRun(task_center=FakeTaskCenter(task=task))
    team_run.cancel_event.set()

    async def runner(_defn, _ctx):
        raise asyncio.CancelledError()

    executor = Executor(
        team_run=team_run,
        runner=runner,
        agent_lookup=lambda name: FakeDefn(),
        build_query_context=AsyncMock(return_value=TeamAgentContext(user_message="ctx")),
    )

    update = await executor.run(task.id)

    assert update.status is TaskStatus.CANCELLED


@pytest.mark.asyncio
async def test_post_dispatch_calls_after_dispatch_hook():
    task = _make_task()
    team_run = FakeTeamRun(task_center=FakeTaskCenter(task=task))
    captured: list[tuple[Task | None, TaskStatusUpdate]] = []

    def hook(t, u):
        captured.append((t, u))

    executor = Executor(
        team_run=team_run,
        runner=AsyncMock(),
        agent_lookup=lambda name: FakeDefn(),
        after_dispatch=hook,
    )
    update = TaskStatusUpdate(task_id=task.id, status=TaskStatus.DONE, summary="ok")

    await executor.post_dispatch(update)

    assert captured == [(task, update)]


# ---------------------------------------------------------------------------
# Scope-change warning injection + context builder
# ---------------------------------------------------------------------------


def test_inject_scope_warnings_posts_note_for_external_scoped_changes():
    created_at = datetime(2026, 4, 12, 12, 0, tzinfo=timezone.utc)
    external_change = SimpleNamespace(
        file_path="src/auth/session.py",
        edit_type="edit",
        agent_run_id="other-run",
        task_id="task-other",
        created_at=datetime(2026, 4, 12, 12, 1, tzinfo=timezone.utc),
    )
    own_change = SimpleNamespace(
        file_path="src/auth/local.py",
        edit_type="edit",
        agent_run_id="agent-run-1",
        task_id="task-1",
        created_at=datetime(2026, 4, 12, 12, 1, tzinfo=timezone.utc),
    )
    arbiter = SimpleNamespace(
        initialized=True,
        changes_since=lambda since, team_run_id=None: [external_change, own_change],
    )

    tc = FakeTaskCenter()
    team_run = FakeTeamRun(task_center=tc, arbiter=arbiter)
    executor = Executor(
        team_run=team_run,
        runner=AsyncMock(),
        agent_lookup=lambda name: FakeDefn(),
    )
    task = _make_task()
    task.created_at = created_at

    asyncio.run(executor.scope_notifier.inject_warning(task))

    assert len(tc.notes) == 1
    assert "src/auth/session.py" in tc.notes[0].content
    assert "src/auth/local.py" not in tc.notes[0].content


def test_inject_scope_warnings_skips_when_store_not_initialized():
    arbiter = SimpleNamespace(
        initialized=False,
        changes_since=lambda since, team_run_id=None: pytest.fail("should not be called"),
    )
    tc = FakeTaskCenter()
    team_run = FakeTeamRun(task_center=tc, arbiter=arbiter)
    executor = Executor(
        team_run=team_run,
        runner=AsyncMock(),
        agent_lookup=lambda name: FakeDefn(),
    )

    asyncio.run(executor.scope_notifier.inject_warning(_make_task()))

    assert tc.notes == []


def test_build_context_uses_override_when_provided():
    team_run = FakeTeamRun()
    defn = FakeDefn()
    expected = TeamAgentContext(user_message="override", tool_metadata={"source": "override"})
    build_query_context = AsyncMock(return_value=expected)
    executor = Executor(
        team_run=team_run,
        runner=AsyncMock(),
        agent_lookup=lambda name: FakeDefn(),
        build_query_context=build_query_context,
    )
    task = _make_task()

    result = asyncio.run(executor._build_context(defn, task))

    assert result is expected
    build_query_context.assert_awaited_once_with(defn, team_run, task)
