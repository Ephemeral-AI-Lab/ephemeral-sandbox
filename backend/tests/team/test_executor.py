"""Unit tests for the Executor post-run, checkpoint, dispatch, and scope
injection in team.runtime.executor.Executor."""

from __future__ import annotations

from datetime import datetime, timezone
from types import SimpleNamespace
from typing import Any
from unittest.mock import AsyncMock

import pytest

from team.errors import GraphInvariantViolation
from team.models import (
    AgentResult,
    Plan,
    ReplanPlan,
    ReplanRequest,
    Task,
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
    def __init__(self, task_center=None, dispatch_queue=None, arbiter=None,
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
        self.arbiter = arbiter

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
        objective="fix the bug",
        scope_paths=["src/auth/"],
        parent_id=parent_id,
        root_id="root-1",
        agent_run_id=agent_run_id,
        created_at=datetime.now(timezone.utc),
        failure_reason=failure_reason,
    )


def _make_executor(
    stats: dict[str, int] | None = None,
    arbiter=None,
) -> tuple[Executor, FakeTaskCenter]:
    tc = FakeTaskCenter()
    team_run = FakeTeamRun(
        task_center=tc,
        arbiter=arbiter,
        stats=stats,
    )
    executor = Executor(
        team_run=team_run,
        runner=AsyncMock(),
        agent_lookup=lambda name: FakeDefn(),
    )
    return executor, tc


# ---------------------------------------------------------------------------
# _read_result tests — read structured result from tool_metadata
# ---------------------------------------------------------------------------


def test_read_result_success_summary():
    """When task_summary_type='success', _read_result returns AgentResult."""
    executor, _ = _make_executor()
    ctx = TeamAgentContext(
        tool_metadata={
            "task_summary_type": "success",
            "task_summary": "Fixed the auth bug in session.py",
        }
    )
    task = SimpleNamespace(id="task-1", agent_name="developer")
    result = executor._read_result(task, ctx)
    assert isinstance(result, AgentResult)
    assert result.summary == "Fixed the auth bug in session.py"


def test_read_result_fail_triggers_replan():
    """When task_summary_type='fail', _read_result returns ReplanRequest."""
    executor, _ = _make_executor()
    ctx = TeamAgentContext(
        tool_metadata={
            "task_summary_type": "fail",
            "task_summary": "Auth spans 3 services, need separate tasks",
        }
    )
    task = SimpleNamespace(id="task-1", agent_name="developer")
    result = executor._read_result(task, ctx)
    assert isinstance(result, ReplanRequest)
    assert "Auth spans 3 services" in result.reason


def test_read_result_planner_submit_plan():
    """When resolved_plan is a Plan, _read_result returns AgentResult with plan."""
    executor, _ = _make_executor()
    plan = Plan.from_dict({"tasks": [{"id": "t1", "objective": "fix it", "agent": "developer"}]})
    ctx = TeamAgentContext(
        tool_metadata={
            "resolved_plan": plan,
            "plan_is_replan": False,
        }
    )
    task = SimpleNamespace(id="task-1", agent_name="team_planner")
    result = executor._read_result(task, ctx)
    assert isinstance(result, AgentResult)
    assert result.submitted_plan is not None
    assert len(result.submitted_plan.tasks) == 1


def test_read_result_replanner_submit_replan():
    """When resolved_plan is a ReplanPlan, _read_result returns AgentResult with replan."""
    executor, _ = _make_executor()
    replan = ReplanPlan.from_dict({
        "add_tasks": [{"id": "t1", "objective": "retry fix", "agent": "developer"}],
        "cancel_ids": ["old-task-1"],
    })
    ctx = TeamAgentContext(
        tool_metadata={
            "resolved_plan": replan,
            "plan_is_replan": True,
        }
    )
    task = SimpleNamespace(id="task-1", agent_name="team_replanner")
    result = executor._read_result(task, ctx)
    assert isinstance(result, AgentResult)
    assert result.submitted_replan is not None
    assert len(result.submitted_replan.add_tasks) == 1
    assert result.submitted_replan.cancel_ids == ["old-task-1"]


def test_read_result_no_submission_fallback():
    """When no terminal tool was called, _read_result returns fallback AgentResult."""
    executor, _ = _make_executor()
    ctx = TeamAgentContext(
        tool_metadata={
            "work_result": "I was still working on something",
        }
    )
    task = SimpleNamespace(id="task-1", agent_name="developer")
    result = executor._read_result(task, ctx)
    assert isinstance(result, AgentResult)
    assert "completed (no submission)" in result.summary
    assert "I was still working" in result.summary


def test_read_result_empty_metadata():
    """When metadata is completely empty, returns fallback."""
    executor, _ = _make_executor()
    ctx = TeamAgentContext(tool_metadata={})
    task = SimpleNamespace(id="task-1", agent_name="developer")
    result = executor._read_result(task, ctx)
    assert isinstance(result, AgentResult)
    assert "completed (no submission)" in result.summary


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
    asyncio.run(executor._post_checkpoint_note(task, None))

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
    out_of_scope_change = SimpleNamespace(
        file_path="src/billing/invoice.py",
        edit_type="edit",
        agent_run_id="other-run",
        task_id="task-other",
        created_at=datetime(2026, 4, 12, 12, 1, tzinfo=timezone.utc),
    )
    arbiter = SimpleNamespace(
        initialized=True,
        changes_since=lambda since, team_run_id=None: [external_change, own_change, out_of_scope_change],
    )

    executor, tc = _make_executor(arbiter=arbiter)
    task = _make_task(agent_run_id="agent-run-1")
    task.created_at = created_at

    asyncio.run(executor._inject_scope_warnings(task))

    assert len(tc.notes) == 1
    assert tc.notes[0].task_id == task.id
    assert "Warning: scope changes detected since plan creation" in tc.notes[0].content
    assert "src/auth/session.py" in tc.notes[0].content
    assert "src/auth/local.py" not in tc.notes[0].content
    assert "src/billing/invoice.py" not in tc.notes[0].content
    assert "system will handle replanning" in tc.notes[0].content


def test_inject_scope_warnings_skips_when_store_not_initialized():
    import asyncio

    arbiter = SimpleNamespace(
        initialized=False,
        changes_since=lambda since, team_run_id=None: pytest.fail("changes_since should not be called"),
    )
    executor, tc = _make_executor(arbiter=arbiter)
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
                status="running", objective="t", description="",
                deps=[], scope_paths=[],
                scope_ltree=[], parent_id=None,
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


def test_run_forever_fails_team_run_on_queue_graph_invariant_violation():
    import asyncio

    class FakeQueue:
        async def pop_ready(self, run_id: str) -> Any:
            raise GraphInvariantViolation("ready task has unfinished deps")

    tc = FakeTaskCenter()
    team_run = FakeTeamRun(task_center=tc, dispatch_queue=FakeQueue())
    team_run.cancel_event = asyncio.Event()
    team_run.fail_fast = AsyncMock()
    executor = Executor(
        team_run=team_run,
        runner=AsyncMock(),
        agent_lookup=lambda name: FakeDefn(),
    )

    asyncio.run(executor.run_forever())

    team_run.fail_fast.assert_awaited_once()
    assert "ready task has unfinished deps" in team_run.fail_fast.await_args.args[0]


def test_run_forever_fails_team_run_on_worker_graph_invariant_violation():
    import asyncio
    from types import SimpleNamespace

    class FakeQueue:
        def __init__(self) -> None:
            self.calls = 0

        async def pop_ready(self, run_id: str) -> Any:
            self.calls += 1
            return SimpleNamespace(
                id="task-1",
                team_run_id=run_id,
                agent_name="dev",
                status="running",
                objective="t",
                description="",
                deps=[],
                scope_paths=[],
                scope_ltree=[],
                parent_id=None,
                root_id="",
                depth=0,
                pending_dep_count=0,
                retry_count=0,
                max_retries=2,
                agent_run_id=None,
                created_at=None,
                started_at=None,
                finished_at=None,
                failure_reason=None,
            )

    tc = FakeTaskCenter()
    tc.graph = {}
    team_run = FakeTeamRun(task_center=tc, dispatch_queue=FakeQueue())
    team_run.cancel_event = asyncio.Event()
    team_run.fail_fast = AsyncMock()
    executor = Executor(
        team_run=team_run,
        runner=AsyncMock(),
        agent_lookup=lambda name: FakeDefn(),
    )
    executor._run_one = AsyncMock(
        side_effect=GraphInvariantViolation("request_replan dependent is ready")
    )

    asyncio.run(executor.run_forever())

    team_run.fail_fast.assert_awaited_once()
    assert "request_replan dependent is ready" in team_run.fail_fast.await_args.args[0]


def test_run_forever_fails_team_run_when_runner_raises_graph_invariant_violation():
    import asyncio
    from types import SimpleNamespace

    class FakeQueue:
        def __init__(self) -> None:
            self.calls = 0

        async def pop_ready(self, run_id: str) -> Any:
            self.calls += 1
            if self.calls > 1:
                team_run.cancel_event.set()
                return None
            return SimpleNamespace(
                id="task-1",
                team_run_id=run_id,
                agent_name="dev",
                status="running",
                objective="t",
                description="",
                deps=[],
                scope_paths=[],
                scope_ltree=[],
                parent_id=None,
                root_id="",
                depth=0,
                pending_dep_count=0,
                retry_count=0,
                max_retries=2,
                agent_run_id=None,
                created_at=None,
                started_at=None,
                finished_at=None,
                failure_reason=None,
            )

    task = _make_task(status="running", parent_id=None, agent_run_id="")
    tc = FakeTaskCenter()
    tc.graph = {}
    tc.mark_running = AsyncMock(return_value=task)
    tc.fail_task = AsyncMock()
    team_run = FakeTeamRun(task_center=tc, dispatch_queue=FakeQueue())
    team_run.cancel_event = asyncio.Event()
    team_run.fail_fast = AsyncMock()

    async def runner(_defn, _ctx) -> None:
        raise GraphInvariantViolation("runner invariant")

    executor = Executor(
        team_run=team_run,
        runner=runner,
        agent_lookup=lambda name: FakeDefn(),
        build_query_context=AsyncMock(return_value=TeamAgentContext(user_message="ctx")),
    )

    asyncio.run(executor.run_forever())

    tc.fail_task.assert_not_awaited()
    team_run.fail_fast.assert_awaited_once()
    assert "runner invariant" in team_run.fail_fast.await_args.args[0]


def test_run_forever_fails_team_run_when_worker_error_cleanup_hits_graph_invariant():
    import asyncio
    from types import SimpleNamespace

    class FakeQueue:
        async def pop_ready(self, run_id: str) -> Any:
            return SimpleNamespace(
                id="task-1",
                team_run_id=run_id,
                agent_name="dev",
                status="running",
                objective="t",
                description="",
                deps=[],
                scope_paths=[],
                scope_ltree=[],
                parent_id=None,
                root_id="",
                depth=0,
                pending_dep_count=0,
                retry_count=0,
                max_retries=2,
                agent_run_id=None,
                created_at=None,
                started_at=None,
                finished_at=None,
                failure_reason=None,
            )

    tc = FakeTaskCenter()
    tc.graph = {}
    tc.fail_task = AsyncMock(
        side_effect=GraphInvariantViolation("retry would run before deps done")
    )
    team_run = FakeTeamRun(task_center=tc, dispatch_queue=FakeQueue())
    team_run.cancel_event = asyncio.Event()
    team_run.fail_fast = AsyncMock()
    executor = Executor(
        team_run=team_run,
        runner=AsyncMock(),
        agent_lookup=lambda name: FakeDefn(),
    )
    executor._run_one = AsyncMock(side_effect=RuntimeError("worker blew up"))

    asyncio.run(executor.run_forever())

    tc.fail_task.assert_awaited_once()
    team_run.fail_fast.assert_awaited_once()
    assert "retry would run before deps done" in team_run.fail_fast.await_args.args[0]


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


# ---------------------------------------------------------------------------
# Budget-exhausted tests (now handled by runner retry loop)
# ---------------------------------------------------------------------------


def test_read_result_budget_exhausted_developer_gets_fail():
    """When budget is exhausted and runner writes fail summary, _read_result
    returns a ReplanRequest (runner writes task_summary_type='fail')."""
    executor, _ = _make_executor()
    # The runner retry loop now handles budget exhaustion by re-prompting
    # the agent. If the agent still can't submit, runner writes a fail summary.
    ctx = TeamAgentContext(
        tool_metadata={
            "task_summary_type": "fail",
            "task_summary": "Agent did not call a submission tool after 5 retries.",
        }
    )
    task = SimpleNamespace(id="task-1", agent_name="developer")
    result = executor._read_result(task, ctx)
    assert isinstance(result, ReplanRequest)
    assert "did not call" in result.reason
