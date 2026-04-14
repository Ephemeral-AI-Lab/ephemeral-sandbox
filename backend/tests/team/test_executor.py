"""Unit tests for the deterministic _posthook() and _post_checkpoint_note()
in team.runtime.executor.Executor."""

from __future__ import annotations

from datetime import datetime, timezone
from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest

from team.models import (
    AgentResult,
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


class FakeTaskCenter:
    """Captures posted notes for assertion."""
    def __init__(self):
        self.notes: list[Note] = []

    async def post(self, note: Note) -> None:
        self.notes.append(note)


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


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _ctx(submitted_output=None, work_result=None) -> TeamAgentContext:
    meta: dict = {}
    if submitted_output is not None:
        meta["submitted_output"] = submitted_output
    if work_result is not None:
        meta["work_result"] = work_result
    return TeamAgentContext(tool_metadata=meta)


# ---------------------------------------------------------------------------
# submitted_output is a Plan
# ---------------------------------------------------------------------------


def test_posthook_with_plan_returns_agent_result_with_submitted_plan():
    plan = Plan(tasks=[TaskSpec(id="t1", task="do it", agent="developer")])
    ctx = _ctx(submitted_output=plan)
    result = Executor._posthook_legacy(ctx, FakeDefn())
    assert isinstance(result, AgentResult)
    assert result.submitted_plan is plan
    assert result.submitted_replan is None


# ---------------------------------------------------------------------------
# submitted_output is a ReplanPlan
# ---------------------------------------------------------------------------


def test_posthook_with_replan_returns_agent_result_with_submitted_replan():
    replan = ReplanPlan(
        add_tasks=[TaskSpec(id="fix", task="fix bug", agent="developer")],
        cancel_ids=["old-1"],
    )
    ctx = _ctx(submitted_output=replan)
    result = Executor._posthook_legacy(ctx, FakeDefn())
    assert isinstance(result, AgentResult)
    assert result.submitted_replan is replan
    assert result.submitted_plan is None


# ---------------------------------------------------------------------------
# submitted_output is a RetryRequest
# ---------------------------------------------------------------------------


def test_posthook_with_retry_request_returns_retry_request_directly():
    retry = RetryRequest(reason="flaky test, retrying")
    ctx = _ctx(submitted_output=retry)
    result = Executor._posthook_legacy(ctx, FakeDefn())
    assert result is retry
    assert isinstance(result, RetryRequest)


# ---------------------------------------------------------------------------
# submitted_output is a ReplanRequest
# ---------------------------------------------------------------------------


def test_posthook_with_replan_request_returns_replan_request_directly():
    replan_req = ReplanRequest(reason="scope mismatch", suggestion="split task")
    ctx = _ctx(submitted_output=replan_req)
    result = Executor._posthook_legacy(ctx, FakeDefn())
    assert result is replan_req
    assert isinstance(result, ReplanRequest)


# ---------------------------------------------------------------------------
# No submission — role-aware fallbacks
# ---------------------------------------------------------------------------


def test_posthook_no_submission_planner_role_returns_sentinel():
    ctx = _ctx()  # no submitted_output
    result = Executor._posthook_legacy(ctx, FakePlannerDefn())
    assert isinstance(result, AgentResult)
    assert result.summary == "planner_did_not_submit_plan"


def test_posthook_no_submission_developer_with_work_result_uses_it():
    ctx = _ctx(work_result="test output here")
    result = Executor._posthook_legacy(ctx, FakeDefn())
    assert isinstance(result, AgentResult)
    assert result.summary == "test output here"


def test_posthook_no_submission_work_result_truncated_to_2000_chars():
    long_result = "A" * 5000
    ctx = _ctx(work_result=long_result)
    result = Executor._posthook_legacy(ctx, FakeDefn())
    assert isinstance(result, AgentResult)
    assert len(result.summary) == 2000
    assert result.summary == "A" * 2000


def test_posthook_no_submission_no_work_result_returns_default():
    ctx = _ctx()
    result = Executor._posthook_legacy(ctx, FakeDefn())
    assert isinstance(result, AgentResult)
    assert result.summary == "completed (no explicit submission)"


def test_posthook_no_submission_empty_work_result_returns_default():
    ctx = _ctx(work_result="   ")  # whitespace only
    result = Executor._posthook_legacy(ctx, FakeDefn())
    assert isinstance(result, AgentResult)
    assert result.summary == "completed (no explicit submission)"


# ---------------------------------------------------------------------------
# Unknown submitted_output type
# ---------------------------------------------------------------------------


def test_posthook_unknown_submitted_type_coerces_to_string():
    ctx = _ctx(submitted_output={"unexpected": "dict"})
    result = Executor._posthook_legacy(ctx, FakeDefn())
    assert isinstance(result, AgentResult)
    assert "unexpected" in result.summary


# ---------------------------------------------------------------------------
# Edge cases with metadata
# ---------------------------------------------------------------------------


def test_posthook_empty_tool_metadata_dict():
    # TeamAgentContext with empty dict
    ctx = TeamAgentContext(tool_metadata={})
    result = Executor._posthook_legacy(ctx, FakeDefn())
    assert isinstance(result, AgentResult)
    assert result.summary == "completed (no explicit submission)"


def test_posthook_plan_has_empty_summary():
    plan = Plan(tasks=[])
    ctx = _ctx(submitted_output=plan)
    result = Executor._posthook_legacy(ctx, FakeDefn())
    assert isinstance(result, AgentResult)
    assert result.summary == ""


# ---------------------------------------------------------------------------
# _post_checkpoint_note tests
# ---------------------------------------------------------------------------


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


def test_run_one_subscribes_and_unsubscribes_scope_listener():
    import asyncio

    class FakeScopeListener:
        def __init__(self):
            self.is_running = True
            self.subscribed: list[tuple[str, list[str]]] = []
            self.unsubscribed: list[str] = []

        def subscribe(self, agent_run_id: str, scope_paths: list[str], _buffer) -> None:
            self.subscribed.append((agent_run_id, scope_paths))

        def unsubscribe(self, agent_run_id: str) -> None:
            self.unsubscribed.append(agent_run_id)

    task = _make_task(status="pending")
    tc = FakeTaskCenter()
    tc.graph = {}
    tc.mark_running = AsyncMock(return_value=task)
    tc.fail = AsyncMock()
    tc.complete_task = AsyncMock(return_value=[])
    tc.sibling_stats = AsyncMock(return_value={"done": 0, "failed": 0, "retry_total": 0})
    team_run = FakeTeamRun(task_center=tc)
    team_run.scope_listener = FakeScopeListener()

    async def runner(_defn, ctx):
        assert "scope_change_buffer" in ctx.tool_metadata.extras

    executor = Executor(
        team_run=team_run,
        runner=runner,
        agent_lookup=lambda name: FakeDefn(),
        build_query_context=AsyncMock(return_value=TeamAgentContext(user_message="ctx")),
    )

    asyncio.run(executor._run_one(task))

    assert len(team_run.scope_listener.subscribed) == 1
    assert team_run.scope_listener.subscribed[0][1] == ["src/auth/"]
    assert len(team_run.scope_listener.unsubscribed) == 1
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


