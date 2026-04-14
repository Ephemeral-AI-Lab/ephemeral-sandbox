"""Unit tests for the Executor post-run, checkpoint, dispatch, and scope
injection in team.runtime.executor.Executor."""

from __future__ import annotations

from datetime import datetime, timezone
from types import SimpleNamespace
from typing import Any
from unittest.mock import AsyncMock

import pytest

from team.models import (
    AgentResult,
    BlockerDeclaration,
    Note,
    Plan,
    ReplanPlan,
    ReplanRequest,
    RetryRequest,
    Task,
    TaskSpec,
    TaskStatus,
)
from team.runtime.context_builder import TeamAgentContext
from team.runtime.executor import Executor


# ---------------------------------------------------------------------------
# Minimal fakes
# ---------------------------------------------------------------------------


class FakeDefn:
    """Minimal agent definition stub."""
    role = "developer"
    name = "developer"


class FakePlannerDefn:
    role = "planner"
    name = "team_planner"


class _NotesProxy(list):
    """List that also exposes ``.post()`` so production code (``tc.notes.post(...)``)
    appends here while tests can still treat ``tc.notes`` as a plain list."""

    async def post(self, note):
        self.append(note)


class FakeTaskCenter:
    """Captures posted notes for assertion."""
    def __init__(self):
        self.notes = _NotesProxy()
        self.store = self  # production reads sibling_stats via tc.store
        self.activity = self  # production routes activity calls via tc.activity


class FakeTeamRun:
    """Minimal team run stub for checkpoint note tests."""
    def __init__(self, task_center=None, dispatch_queue=None, file_change_store=None,
                 stats=None):
        self.id = "test-run-001"
        _default_stats = stats or {"done": 0, "failed": 0, "pending": 0,
                                   "ready": 0, "running": 0, "cancelled": 0,
                                   "retry_total": 0}
        tc = task_center or FakeTaskCenter()
        if not hasattr(tc, "sibling_stats"):
            tc.sibling_stats = self._make_sibling_stats(_default_stats)
        self.task_center = tc
        self.dispatch_queue = dispatch_queue
        self.file_change_store = file_change_store

    @staticmethod
    def _make_sibling_stats(stats):
        async def sibling_stats(parent_id):
            return dict(stats)
        return sibling_stats

    async def checkpoint(self, label: str = "") -> None:
        pass


def _make_task(
    *,
    status: str = "done",
    parent_id: str | None = "parent-1",
    failure_reason: str | None = None,
    agent_run_id: str = "agent-run-1",
) -> Task:
    return Task(
        id="task-1",
        team_run_id="test-run-001",
        agent_name="developer",
        status=TaskStatus(status),
        task="fix the bug",
        scope_paths=["src/auth/"],
        parent_id=parent_id,
        root_id="root-1",
        agent_run_id=agent_run_id,
        created_at=datetime.now(timezone.utc),
        failure_reason=failure_reason,
    )


def _make_executor(
    stats: dict[str, int] | None = None,
    file_change_store=None,
) -> tuple[Executor, FakeTaskCenter]:
    tc = FakeTaskCenter()
    team_run = FakeTeamRun(
        task_center=tc,
        file_change_store=file_change_store,
        stats=stats,
    )
    executor = Executor(
        team_run=team_run,
        runner=AsyncMock(),
        agent_lookup=lambda name: FakeDefn(),
    )
    return executor, tc


# ---------------------------------------------------------------------------
# _run_post_run tests — posthook re-prompt via external trigger
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_run_post_run_uses_external_trigger_agent(monkeypatch):
    captured: dict[str, object] = {}

    async def fake_run_external_trigger(**kwargs):
        captured.update(kwargs)
        return SimpleNamespace(
            tool_name="post_note",
            tool_input={"content": "done"},
            validated=None,
            turns_used=1,
        )

    monkeypatch.setattr("external_trigger.runner.run", fake_run_external_trigger)

    executor, _ = _make_executor()
    executor.team_run.api_client = object()
    executor.team_run.conductor = SimpleNamespace(
        _executor_snapshots={"task-1": [{"role": "assistant", "content": "frozen"}]},
    )
    ctx = TeamAgentContext(
        tool_metadata={
            "agent_name": "developer",
            "role": "developer",
            "posthook_prompt": "Submit your result.",
            "work_result": "Developer summary",
        }
    )

    result = await executor._run_post_run(
        task=SimpleNamespace(id="task-1", agent_name="developer"),
        defn=FakeDefn(),
        ctx=ctx,
    )

    assert isinstance(result, AgentResult)
    assert result.summary == "done"
    assert captured["agent_name"] == "posthook:developer:task-1"
    assert captured["messages"] == [{"role": "assistant", "content": "frozen"}]
    assert "Developer summary" in str(captured["prompt"])
    assert [tool.name for tool in captured["tools"]] == ["post_note", "request_replan"]
    assert captured["execute_tools"] is True


@pytest.mark.asyncio
async def test_run_post_run_planner_submit_plan(monkeypatch):
    captured: dict[str, object] = {}

    async def fake_run_external_trigger(**kwargs):
        captured.update(kwargs)
        return SimpleNamespace(
            tool_name="submit_plan",
            tool_input={"tasks": [{"id": "t1", "task": "fix it", "agent": "developer"}]},
            validated=None,
            turns_used=1,
        )

    monkeypatch.setattr("external_trigger.runner.run", fake_run_external_trigger)

    executor, _ = _make_executor()
    executor.team_run.api_client = object()
    executor.team_run.conductor = SimpleNamespace(
        _executor_snapshots={"task-1": [{"role": "assistant", "content": "frozen"}]},
    )
    ctx = TeamAgentContext(
        tool_metadata={
            "agent_name": "team_planner",
            "role": "planner",
            "posthook_prompt": "Submit your plan.",
            "work_result": "Legacy plan text without parseable JSON",
        }
    )

    result = await executor._run_post_run(
        task=SimpleNamespace(id="task-1", agent_name="team_planner"),
        defn=FakePlannerDefn(),
        ctx=ctx,
    )

    assert isinstance(result, AgentResult)
    assert result.submitted_plan is not None
    assert len(result.submitted_plan.tasks) == 1
    assert captured["agent_name"] == "posthook:team_planner:task-1"
    assert captured["execute_tools"] is True


@pytest.mark.asyncio
async def test_run_post_run_prefers_resolved_plan_metadata(monkeypatch):
    resolved_plan = Plan.from_dict(
        {"tasks": [{"id": "t1", "task": "fix it", "agent": "validator"}]}
    )

    async def fake_run_external_trigger(**kwargs):
        del kwargs
        return SimpleNamespace(
            tool_name="submit_plan",
            tool_input={"tasks": [{"id": "", "task": "broken raw payload", "agent": ""}]},
            tool_result=SimpleNamespace(metadata={"resolved_plan": resolved_plan}),
            validated=None,
            turns_used=2,
        )

    monkeypatch.setattr("external_trigger.runner.run", fake_run_external_trigger)

    executor, _ = _make_executor()
    executor.team_run.api_client = object()
    executor.team_run.conductor = SimpleNamespace(
        _executor_snapshots={"task-1": [{"role": "assistant", "content": "frozen"}]},
    )
    ctx = TeamAgentContext(
        tool_metadata={
            "agent_name": "team_planner",
            "role": "planner",
            "posthook_prompt": "Submit your plan.",
            "work_result": "Legacy plan text without parseable JSON",
        }
    )

    result = await executor._run_post_run(
        task=SimpleNamespace(id="task-1", agent_name="team_planner"),
        defn=FakePlannerDefn(),
        ctx=ctx,
    )

    assert isinstance(result, AgentResult)
    assert result.submitted_plan is not None
    assert result.submitted_plan.tasks[0].agent == "validator"


@pytest.mark.asyncio
async def test_run_post_run_no_api_client_returns_sentinel(monkeypatch):
    executor, _ = _make_executor()
    executor.team_run.api_client = None
    ctx = TeamAgentContext(tool_metadata={"role": "developer"})

    result = await executor._run_post_run(
        task=SimpleNamespace(id="task-1", agent_name="developer"),
        defn=FakeDefn(),
        ctx=ctx,
    )

    assert isinstance(result, AgentResult)
    assert "no api_client" in result.summary


@pytest.mark.asyncio
async def test_run_post_run_runner_failure_returns_sentinel(monkeypatch):
    async def failing_trigger(**kwargs):
        raise RuntimeError("LLM down")

    monkeypatch.setattr("external_trigger.runner.run", failing_trigger)

    executor, _ = _make_executor()
    executor.team_run.api_client = object()
    executor.team_run.conductor = SimpleNamespace(_executor_snapshots={})
    ctx = TeamAgentContext(tool_metadata={"role": "developer"})

    result = await executor._run_post_run(
        task=SimpleNamespace(id="task-1", agent_name="developer"),
        defn=FakeDefn(),
        ctx=ctx,
    )

    assert isinstance(result, AgentResult)
    assert "posthook runner failed" in result.summary


# ---------------------------------------------------------------------------
# _post_checkpoint_note tests
# ---------------------------------------------------------------------------


def test_checkpoint_note_posted_on_completion():
    """A checkpoint note is posted after every task dispatch."""
    import asyncio
    executor, tc = _make_executor(stats={"done": 1, "failed": 0,
                                         "pending": 0, "ready": 0,
                                         "running": 0, "cancelled": 0,
                                         "retry_total": 0})
    task = _make_task()
    result = AgentResult(summary="all good")
    action = asyncio.run(executor._post_checkpoint_note(task, result))

    assert action is None
    assert len(tc.notes) == 1
    assert "Checkpoint: task-1" in tc.notes[0].content
    assert tc.notes[0].agent_name == "checkpoint"
    # Note is attributed to parent_id so it flows through parent chain
    # reads, not dep reads (avoids shadowing the task's done() summary).
    assert tc.notes[0].task_id == "parent-1"


def test_checkpoint_note_includes_failure_reason():
    import asyncio
    executor, tc = _make_executor(stats={"done": 0, "failed": 1,
                                         "pending": 0, "ready": 0,
                                         "running": 0, "cancelled": 0,
                                         "retry_total": 0})
    task = _make_task(status="failed", failure_reason="sandbox timeout")
    action = asyncio.run(executor._post_checkpoint_note(task, None))

    assert len(tc.notes) == 1
    assert "sandbox timeout" in tc.notes[0].content


def test_checkpoint_note_critical_when_high_failure_rate():
    """When >40% of 3+ started siblings failed, action is 'replan'."""
    import asyncio
    executor, tc = _make_executor(stats={"done": 1, "failed": 2,
                                         "pending": 0, "ready": 0,
                                         "running": 0, "cancelled": 0,
                                         "retry_total": 0})
    task = _make_task(status="failed")
    action = asyncio.run(executor._post_checkpoint_note(task, None))

    assert action == "replan"
    assert "PLAN HEALTH CRITICAL" in tc.notes[0].content
    assert "2/3" in tc.notes[0].content


def test_checkpoint_note_no_critical_below_threshold():
    """1 failure out of 3 (33%) should NOT trigger replan."""
    import asyncio
    executor, tc = _make_executor(stats={"done": 2, "failed": 1,
                                         "pending": 0, "ready": 0,
                                         "running": 0, "cancelled": 0,
                                         "retry_total": 0})
    task = _make_task(status="failed")
    action = asyncio.run(executor._post_checkpoint_note(task, None))

    assert action is None
    assert "PLAN HEALTH CRITICAL" not in tc.notes[0].content


def test_checkpoint_note_no_critical_when_few_started():
    """Even 100% failure rate with <3 started should NOT trigger."""
    import asyncio
    executor, tc = _make_executor(stats={"done": 0, "failed": 2,
                                         "pending": 3, "ready": 0,
                                         "running": 0, "cancelled": 0,
                                         "retry_total": 0})
    task = _make_task(status="failed")
    action = asyncio.run(executor._post_checkpoint_note(task, None))

    assert action is None


def test_checkpoint_note_retry_warning():
    """3+ retries across siblings triggers a warning."""
    import asyncio
    executor, tc = _make_executor(stats={"done": 2, "failed": 0,
                                         "pending": 1, "ready": 0,
                                         "running": 0, "cancelled": 0,
                                         "retry_total": 4})
    task = _make_task()
    action = asyncio.run(executor._post_checkpoint_note(task, None))

    assert action is None  # warning, not replan
    assert "PLAN HEALTH WARNING" in tc.notes[0].content
    assert "4 retries" in tc.notes[0].content


def test_checkpoint_note_survives_sibling_stats_error():
    """If sibling_stats raises, checkpoint note is skipped gracefully."""
    import asyncio
    tc = FakeTaskCenter()
    tc.sibling_stats = AsyncMock(side_effect=RuntimeError("db down"))
    team_run = FakeTeamRun(task_center=tc)
    executor = Executor(team_run=team_run, runner=AsyncMock(),
                        agent_lookup=lambda n: FakeDefn())

    task = _make_task()
    action = asyncio.run(executor._post_checkpoint_note(task, None))

    assert action is None
    assert len(tc.notes) == 0  # no note posted, no crash


def test_inject_scope_warnings_posts_note_for_external_scoped_changes():
    import asyncio

    created_at = datetime(2026, 4, 12, 12, 0, tzinfo=timezone.utc)
    external_change = SimpleNamespace(
        file_path="src/auth/session.py",
        edit_type="edit",
        agent_id="other-agent",
        agent_run_id="other-run",
        created_at=datetime(2026, 4, 12, 12, 1, tzinfo=timezone.utc),
    )
    own_change = SimpleNamespace(
        file_path="src/auth/local.py",
        edit_type="edit",
        agent_id="developer",
        agent_run_id="agent-run-1",
        created_at=datetime(2026, 4, 12, 12, 1, tzinfo=timezone.utc),
    )
    out_of_scope_change = SimpleNamespace(
        file_path="src/billing/invoice.py",
        edit_type="edit",
        agent_id="other-agent",
        agent_run_id="other-run",
        created_at=datetime(2026, 4, 12, 12, 1, tzinfo=timezone.utc),
    )
    file_change_store = SimpleNamespace(
        initialized=True,
        changes_since=lambda since: [external_change, own_change, out_of_scope_change],
    )

    executor, tc = _make_executor(file_change_store=file_change_store)
    task = _make_task(agent_run_id="agent-run-1")
    task.created_at = created_at

    asyncio.run(executor._inject_scope_warnings(task))

    assert len(tc.notes) == 1
    assert tc.notes[0].task_id == task.id
    assert "Warning: scope changes detected since plan creation" in tc.notes[0].content
    assert "src/auth/session.py" in tc.notes[0].content
    assert "src/auth/local.py" not in tc.notes[0].content
    assert "src/billing/invoice.py" not in tc.notes[0].content
    assert "Call request_replan()" in tc.notes[0].content


def test_inject_scope_warnings_skips_when_store_not_initialized():
    import asyncio

    file_change_store = SimpleNamespace(
        initialized=False,
        changes_since=lambda since: pytest.fail("changes_since should not be called"),
    )
    executor, tc = _make_executor(file_change_store=file_change_store)
    task = _make_task()

    asyncio.run(executor._inject_scope_warnings(task))

    assert tc.notes == []


def test_build_context_uses_override_when_provided():
    import asyncio

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


def test_run_one_does_not_set_up_scope_buffer():
    """After Option B unification, executor no longer creates ScopeChangeBuffer."""
    import asyncio

    task = _make_task(status="pending")
    tc = FakeTaskCenter()
    tc.graph = {}
    tc.mark_running = AsyncMock(return_value=task)
    tc.fail = AsyncMock()
    tc.complete_task = AsyncMock(return_value=[])
    tc.sibling_stats = AsyncMock(return_value={"done": 0, "failed": 0, "retry_total": 0})
    team_run = FakeTeamRun(task_center=tc)

    async def runner(_defn, ctx):
        assert "scope_change_buffer" not in ctx.tool_metadata.extras

    executor = Executor(
        team_run=team_run,
        runner=runner,
        agent_lookup=lambda name: FakeDefn(),
        build_query_context=AsyncMock(return_value=TeamAgentContext(user_message="ctx")),
    )

    asyncio.run(executor._run_one(task))
    tc.fail.assert_not_called()


def test_run_forever_survives_transient_pop_ready_error():
    import asyncio
    from types import SimpleNamespace

    class FakeQueue:
        def __init__(self) -> None:
            self.calls = 0

        async def pop_ready(self, run_id: str) -> Any:
            self.calls += 1
            if self.calls == 1:
                raise RuntimeError("db down")
            # Return a fake record that _record_to_task can handle
            return SimpleNamespace(
                id="task-1", team_run_id=run_id, agent_name="dev",
                status="running", task="t", deps=[], scope_paths=[],
                scope_ltree=[], cascade_policy="cancel", parent_id=None,
                root_id="", depth=0, pending_dep_count=0,
                retry_count=0, max_retries=2, agent_run_id=None,
                created_at=None, started_at=None, finished_at=None,
                failure_reason=None,
            )

    fake_queue = FakeQueue()
    tc = FakeTaskCenter()
    tc.graph = {}
    tc.sibling_stats = AsyncMock(return_value={})
    team_run = FakeTeamRun(task_center=tc, dispatch_queue=fake_queue)
    team_run.cancel_event = asyncio.Event()
    executor = Executor(
        team_run=team_run,
        runner=AsyncMock(),
        agent_lookup=lambda name: FakeDefn(),
    )

    async def _run_one(task) -> None:
        team_run.cancel_event.set()

    executor._run_one = AsyncMock(side_effect=_run_one)

    asyncio.run(executor.run_forever())

    assert fake_queue.calls >= 2
    executor._run_one.assert_awaited_once()


def test_dispatch_blocker_declaration_creates_blocker_and_completes_task():
    import asyncio

    task = _make_task(status="running")
    tc = FakeTaskCenter()
    tc.complete_task = AsyncMock(return_value=[])
    team_run = FakeTeamRun(task_center=tc)
    team_run.conductor = SimpleNamespace(
        blocker_for_fix_task=lambda task_id: None,
        create_blocker=AsyncMock(),
    )
    executor = Executor(
        team_run=team_run,
        runner=AsyncMock(),
        agent_lookup=lambda name: FakeDefn(),
    )
    declaration = BlockerDeclaration(
        root_cause_paths=["src/auth/session.py"],
        reason="shared auth helper is broken",
        suggestion="repair helper before resuming sibling work",
    )

    asyncio.run(executor._dispatch(task, declaration))

    team_run.conductor.create_blocker.assert_awaited_once_with(
        reason="shared auth helper is broken",
        root_cause_paths=["src/auth/session.py"],
        initiating_task_id=task.id,
        declared_by=task.id,
    )
    tc.complete_task.assert_awaited_once()
    completed_result = tc.complete_task.await_args.args[1]
    assert isinstance(completed_result, AgentResult)
    assert completed_result.summary == "Declared blocker: shared auth helper is broken"


# ---------------------------------------------------------------------------
# _plan_health_prefix tests
# ---------------------------------------------------------------------------


def test_plan_health_prefix_returns_none_when_healthy():
    import asyncio
    executor, _ = _make_executor(stats={"done": 3, "failed": 0,
                                        "pending": 0, "ready": 0,
                                        "running": 0, "cancelled": 0,
                                        "retry_total": 0})
    task = _make_task()
    prefix = asyncio.run(executor._plan_health_prefix(task))
    assert prefix is None


def test_plan_health_prefix_critical_on_high_failure():
    import asyncio
    executor, _ = _make_executor(stats={"done": 1, "failed": 2,
                                        "pending": 0, "ready": 0,
                                        "running": 0, "cancelled": 0,
                                        "retry_total": 0})
    task = _make_task()
    prefix = asyncio.run(executor._plan_health_prefix(task))
    assert prefix is not None
    assert "PLAN HEALTH CRITICAL" in prefix
    assert "2/3" in prefix


def test_plan_health_prefix_none_when_no_parent():
    """Root tasks have no siblings — skip health check."""
    import asyncio
    executor, _ = _make_executor()
    task = _make_task(parent_id=None)
    prefix = asyncio.run(executor._plan_health_prefix(task))
    assert prefix is None


def test_plan_health_prefix_retry_warning():
    import asyncio
    executor, _ = _make_executor(stats={"done": 2, "failed": 0,
                                        "pending": 0, "ready": 0,
                                        "running": 0, "cancelled": 0,
                                        "retry_total": 5})
    task = _make_task()
    prefix = asyncio.run(executor._plan_health_prefix(task))
    assert prefix is not None
    assert "PLAN HEALTH WARNING" in prefix
    assert "5 retries" in prefix
