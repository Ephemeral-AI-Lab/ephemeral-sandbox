"""Unit tests for the Executor post-run, checkpoint, dispatch, and scope
injection in team.runtime.executor.Executor."""

from __future__ import annotations

import asyncio
from datetime import datetime, timezone
from types import SimpleNamespace
from typing import Any
from unittest.mock import AsyncMock

import pytest

from team.errors import BudgetExceeded, GraphInvariantViolation
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
from team.runtime.team_run import TeamRun


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


class FakeValidatorDefn:
    role = "reviewer"
    name = "validator"


class _NotesProxy(list):
    """List that also exposes ``.post()`` so production code (``tc.notes.post(...)``)
    appends here while tests can still treat ``tc.notes`` as a plain list."""

    async def post(self, note):
        self.append(note)


class FakeTaskCenter:
    """Captures posted notes for assertion."""
    def __init__(self):
        self.notes = _NotesProxy()
        self.activity = self  # production routes activity calls via tc.activity


class FakeTeamRun:
    """Minimal team run stub for executor tests."""
    def __init__(self, task_center=None, dispatch_queue=None, arbiter=None):
        self.id = "test-run-001"
        tc = task_center or FakeTaskCenter()
        self.task_center = tc
        self.dispatch_queue = dispatch_queue
        self.arbiter = arbiter
        self._active_agent_runs = {}

    def register_agent_run(self, task_id: str, runner_task) -> None:
        self._active_agent_runs[task_id] = runner_task

    def unregister_agent_run(self, task_id: str, runner_task) -> None:
        current = self._active_agent_runs.get(task_id)
        if current is runner_task:
            self._active_agent_runs.pop(task_id, None)

    async def checkpoint(self, label: str = "") -> None:
        pass

    async def fail_after_active_work(self, reason: str) -> None:
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
    arbiter=None,
) -> tuple[Executor, FakeTaskCenter]:
    tc = FakeTaskCenter()
    team_run = FakeTeamRun(
        task_center=tc,
        arbiter=arbiter,
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


@pytest.mark.asyncio
async def test_validator_fail_summary_dispatches_replan_request():
    """A validator's fail terminal summary requests replanning at runtime."""
    task = Task(
        id="validator-task",
        team_run_id="test-run-001",
        agent_name="validator",
        status=TaskStatus.READY,
        objective="Validate the implementation.",
        scope_paths=["src/auth/session.py"],
        parent_id="parent-1",
        root_id="root-1",
    )

    class _TaskCenter(FakeTaskCenter):
        def __init__(self):
            super().__init__()
            self.graph = {task.id: task}
            self.replan_requests: list[tuple[str, ReplanRequest]] = []

        async def mark_running(self, task_id: str, agent_run_id: str) -> Task:
            assert task_id == task.id
            task.status = TaskStatus.RUNNING
            task.agent_run_id = agent_run_id
            return task

        async def request_replan(self, task_id: str, request: ReplanRequest) -> None:
            self.replan_requests.append((task_id, request))
            task.status = TaskStatus.REQUEST_REPLAN
            task.failure_reason = request.reason

        async def complete_task(self, task_id: str, result: AgentResult):
            pytest.fail(f"validator failure should not complete task {task_id}: {result}")

    tc = _TaskCenter()
    ctx = TeamAgentContext(user_message="validate", tool_metadata={})

    async def _build_context(_defn, _team_run, _task) -> TeamAgentContext:
        return ctx

    async def _runner(_defn, run_ctx: TeamAgentContext) -> None:
        run_ctx.tool_metadata["task_summary_type"] = "fail"
        run_ctx.tool_metadata["task_summary"] = (
            "FAIL: pytest tests/test_auth.py::test_session still red. "
            "Owner spans auth/session.py and auth/cache.py; needs replanning."
        )

    executor = Executor(
        team_run=FakeTeamRun(task_center=tc),
        runner=_runner,
        agent_lookup=lambda name: FakeValidatorDefn() if name == "validator" else None,
        build_query_context=_build_context,
    )

    await executor._run_one_inner(task)

    assert len(tc.replan_requests) == 1
    task_id, request = tc.replan_requests[0]
    assert task_id == task.id
    assert "needs replanning" in request.reason
    assert task.status == TaskStatus.REQUEST_REPLAN
    assert task.failure_reason == request.reason


@pytest.mark.asyncio
async def test_replan_budget_exhaustion_fails_task_without_fail_fast():
    """Replan budget exhaustion must not cancel unrelated active agent turns."""
    task = _make_task(status="running")
    tc = FakeTaskCenter()
    tc.request_replan = AsyncMock(side_effect=BudgetExceeded("max_replans_per_run reached"))
    tc.fail_task = AsyncMock()

    team_run = FakeTeamRun(task_center=tc)
    team_run.fail_fast = AsyncMock()
    team_run.fail_after_active_work = AsyncMock()

    executor = Executor(
        team_run=team_run,
        runner=AsyncMock(),
        agent_lookup=lambda name: FakeDefn(),
    )

    await executor._dispatch(task, ReplanRequest(reason="needs corrective work"))

    tc.request_replan.assert_awaited_once()
    tc.fail_task.assert_awaited_once_with(
        task.id,
        "replan_budget_exhausted: max_replans_per_run reached",
    )
    team_run.fail_after_active_work.assert_awaited_once_with(
        "replan_budget_exhausted: max_replans_per_run reached"
    )
    team_run.fail_fast.assert_not_awaited()


@pytest.mark.asyncio
async def test_fail_after_active_work_does_not_cancel_active_runners():
    """Graceful run failure stops new work while preserving active submissions."""
    sleeper = asyncio.create_task(asyncio.sleep(60))
    task_center = SimpleNamespace(cancel_all_pending=AsyncMock())
    event_store: list[Any] = []
    team_run = TeamRun.__new__(TeamRun)
    team_run.id = "run-1"
    team_run._fatal_failure_reason = None
    team_run.status = None
    team_run.event_store = event_store
    team_run.cancel_event = asyncio.Event()
    team_run._active_agent_runs = {"task-1": sleeper}
    team_run.task_center = task_center

    try:
        await team_run.fail_after_active_work("replan_budget_exhausted: max")

        assert team_run.cancel_event.is_set()
        assert not sleeper.done()
        task_center.cancel_all_pending.assert_awaited_once()
        assert event_store[-1].kind == "team_run_status"
        assert event_store[-1].data["status"] == "failed"
    finally:
        sleeper.cancel()
        with pytest.raises(asyncio.CancelledError):
            await sleeper


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
        "add_tasks": [{"id": "t1", "objective": "repair fix", "agent": "developer"}],
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


def test_read_result_no_submission_fails():
    """When no terminal tool was called, _read_result returns a ReplanRequest."""
    executor, _ = _make_executor()
    ctx = TeamAgentContext(
        tool_metadata={
            "work_result": "I was still working on something",
        }
    )
    task = SimpleNamespace(id="task-1", agent_name="developer")
    result = executor._read_result(task, ctx)
    assert isinstance(result, ReplanRequest)
    assert "terminal submission tool" in result.reason
    assert "I was still working" in result.reason


def test_read_result_empty_metadata():
    """When metadata is completely empty, returns a ReplanRequest."""
    executor, _ = _make_executor()
    ctx = TeamAgentContext(tool_metadata={})
    task = SimpleNamespace(id="task-1", agent_name="developer")
    result = executor._read_result(task, ctx)
    assert isinstance(result, ReplanRequest)
    assert "terminal submission tool" in result.reason


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
    tc.request_replan = AsyncMock()
    tc.complete_task = AsyncMock(return_value=[])
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
    tc.request_replan.assert_awaited_once()


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
                root_id="", depth=0,
                agent_run_id=None,
                created_at=None, started_at=None, finished_at=None,
                failure_reason=None,
            )

    fake_queue = FakeQueue()
    tc = FakeTaskCenter()
    tc.graph = {}
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
                agent_run_id=None,
                created_at=None,
                started_at=None,
                finished_at=None,
                failure_reason=None,
            )

    tc = FakeTaskCenter()
    tc.graph = {}
    tc.fail_task = AsyncMock(
        side_effect=GraphInvariantViolation("failure cleanup saw unsatisfied deps")
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
    assert "failure cleanup saw unsatisfied deps" in team_run.fail_fast.await_args.args[0]


# ---------------------------------------------------------------------------
# Missing-submission tests
# ---------------------------------------------------------------------------


def test_read_result_missing_submission_gets_fail():
    """When no terminal submission is present, _read_result returns a ReplanRequest."""
    executor, _ = _make_executor()
    ctx = TeamAgentContext(
        tool_metadata={
            "task_summary_type": "fail",
            "task_summary": "Agent did not call a terminal submission tool.",
        }
    )
    task = SimpleNamespace(id="task-1", agent_name="developer")
    result = executor._read_result(task, ctx)
    assert isinstance(result, ReplanRequest)
    assert "did not call" in result.reason
