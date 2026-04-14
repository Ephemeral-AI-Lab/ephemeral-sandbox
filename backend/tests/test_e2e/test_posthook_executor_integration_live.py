# ruff: noqa
"""Live e2e tests — Suite 3: Executor _run_post_run integration.

Tests the full executor post-run flow end-to-end:

  1. Legacy metadata path — agent submitted during query loop, _run_post_run
     honours metadata directly without calling the streaming runner.
  2. Streaming runner fallback — no metadata submission, _run_post_run invokes
     run_trigger() with posthook tools and maps RunResult → domain objects.
  3. Result mapping — each tool_name maps to the correct domain type
     (Plan, ReplanPlan, RetryRequest, ReplanRequest, BlockerDeclaration).
  4. Streaming runner exhaustion — falls back to legacy when runner fails.

Uses real LLM for the streaming runner tests, fakes for metadata-only tests.

Run with:
    .venv/bin/python -m pytest backend/tests/test_e2e/test_posthook_executor_integration_live.py -v -m live -o "addopts="
"""

from __future__ import annotations

import asyncio
import uuid
from dataclasses import dataclass, field
from typing import Any

import pytest

from agents.types import AgentDefinition
from engine.testing.eval_agent import EvalAgent
from team.models import (
    AgentResult,
    BlockerDeclaration,
    Plan,
    ReplanPlan,
    ReplanRequest,
    RetryRequest,
)
from team.runtime.context_builder import TeamAgentContext
from team.runtime.executor import Executor
from tests.test_e2e.conftest import create_eval_agent
from tools.core.base import ExecutionMetadata

pytestmark = [pytest.mark.e2e, pytest.mark.live]

HAS_CREDENTIALS = EvalAgent.has_credentials()


# ---------------------------------------------------------------------------
# Minimal fakes — just enough to call _run_post_run directly
# ---------------------------------------------------------------------------


class FakeTeamRun:
    """Minimal team run stub for _run_post_run tests."""

    def __init__(self, api_client: Any = None) -> None:
        self.id = f"test-run-{uuid.uuid4().hex[:8]}"
        self.api_client = api_client
        self.conductor = None
        self.cancel_event = asyncio.Event()


def _make_ctx(
    *,
    role: str = "developer",
    agent_name: str = "developer",
    submitted_output: Any = None,
    work_result: str | None = None,
) -> TeamAgentContext:
    """Build a TeamAgentContext with the given metadata.

    PosthookTools.from_context reads ``ctx.metadata`` (not ``ctx.tool_metadata``),
    so we attach the ExecutionMetadata as both ``tool_metadata`` (for the executor's
    _posthook_legacy) and ``metadata`` (for PosthookTools.from_context role resolution).
    """
    meta = ExecutionMetadata()
    meta.extras["role"] = role
    meta.agent_name = agent_name
    meta.extras["agent_name"] = agent_name
    if submitted_output is not None:
        meta.extras["submitted_output"] = submitted_output
    if work_result is not None:
        meta.extras["work_result"] = work_result
    ctx = TeamAgentContext(
        user_message="test task",
        tool_metadata=meta,
    )
    # PosthookTools.from_context(ctx) reads getattr(ctx, "metadata", {}).
    # TeamAgentContext only exposes tool_metadata, so alias it here.
    ctx.metadata = meta  # type: ignore[attr-defined]
    return ctx


def _make_defn(
    *,
    name: str = "developer",
    role: str = "developer",
    posthook: list[str] | None = None,
) -> AgentDefinition:
    return AgentDefinition(
        name=name,
        description=f"Test {name} agent",
        role=role,
        posthook=posthook or [],
    )


def _make_executor(api_client: Any = None) -> Executor:
    team_run = FakeTeamRun(api_client=api_client)
    return Executor(
        team_run=team_run,
        runner=lambda defn, ctx: asyncio.sleep(0),
        agent_lookup=lambda name: _make_defn(name=name),
    )


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------


@pytest.fixture(scope="module")
def agent():
    if not HAS_CREDENTIALS:
        pytest.skip("No LLM credentials configured")
    return create_eval_agent()


@pytest.fixture(scope="module")
def api_client(agent):
    return agent.api_client


# ---------------------------------------------------------------------------
# Test 1: Legacy path — Plan already submitted in metadata
# ---------------------------------------------------------------------------


@pytest.mark.skipif(not HAS_CREDENTIALS, reason="No credentials")
@pytest.mark.asyncio
async def test_legacy_honours_submitted_plan():
    """When agent submitted a Plan during query loop, _run_post_run returns it directly."""
    plan = Plan.from_dict({
        "tasks": [
            {"id": "t1", "task": "Fix imports", "agent": "developer", "deps": [], "scope_paths": ["pkg/io.py"]},
            {"id": "t2", "task": "Run tests", "agent": "developer", "deps": ["t1"], "scope_paths": ["pkg/tests/"]},
        ],
    })
    ctx = _make_ctx(role="planner", agent_name="team_planner", submitted_output=plan)
    defn = _make_defn(name="team_planner", role="planner")
    executor = _make_executor()  # no api_client needed — should not reach runner

    result = await executor._run_post_run(task=None, defn=defn, ctx=ctx)

    assert isinstance(result, AgentResult)
    assert result.submitted_plan is not None
    assert len(result.submitted_plan.tasks) == 2


# ---------------------------------------------------------------------------
# Test 2: Legacy path — ReplanRequest already submitted
# ---------------------------------------------------------------------------


@pytest.mark.skipif(not HAS_CREDENTIALS, reason="No credentials")
@pytest.mark.asyncio
async def test_legacy_honours_submitted_replan_request():
    """When agent submitted a ReplanRequest during query loop, _run_post_run returns it."""
    replan_req = ReplanRequest(reason="task is scoped to wrong file", suggestion="target middleware.py instead")
    ctx = _make_ctx(submitted_output=replan_req)
    defn = _make_defn()
    executor = _make_executor()

    result = await executor._run_post_run(task=None, defn=defn, ctx=ctx)

    assert isinstance(result, ReplanRequest)
    assert "wrong file" in result.reason


# ---------------------------------------------------------------------------
# Test 3: Legacy path — BlockerDeclaration already submitted
# ---------------------------------------------------------------------------


@pytest.mark.skipif(not HAS_CREDENTIALS, reason="No credentials")
@pytest.mark.asyncio
async def test_legacy_honours_submitted_blocker():
    """When agent submitted a BlockerDeclaration during query loop, _run_post_run returns it."""
    blocker = BlockerDeclaration(
        root_cause_paths=["pkg/_compat.py"],
        reason="load_defaults renamed, all importers broken",
    )
    ctx = _make_ctx(submitted_output=blocker)
    defn = _make_defn()
    executor = _make_executor()

    result = await executor._run_post_run(task=None, defn=defn, ctx=ctx)

    assert isinstance(result, BlockerDeclaration)
    assert "pkg/_compat.py" in result.root_cause_paths


# ---------------------------------------------------------------------------
# Test 4: Legacy path — RetryRequest already submitted
# ---------------------------------------------------------------------------


@pytest.mark.skipif(not HAS_CREDENTIALS, reason="No credentials")
@pytest.mark.asyncio
async def test_legacy_honours_submitted_retry():
    """When agent submitted a RetryRequest during query loop, _run_post_run returns it."""
    retry = RetryRequest(reason="sandbox timeout, no code changes made")
    ctx = _make_ctx(submitted_output=retry)
    defn = _make_defn()
    executor = _make_executor()

    result = await executor._run_post_run(task=None, defn=defn, ctx=ctx)

    assert isinstance(result, RetryRequest)
    assert "timeout" in result.reason


# ---------------------------------------------------------------------------
# Test 5: Streaming runner — developer post_note (real LLM)
# ---------------------------------------------------------------------------


@pytest.mark.skipif(not HAS_CREDENTIALS, reason="No credentials")
@pytest.mark.asyncio
async def test_streaming_runner_developer_post_note(api_client):
    """No metadata submission → runner invokes LLM with developer posthook tools → post_note."""
    ctx = _make_ctx(role="developer", agent_name="developer")
    # Inject conversation snapshot so the LLM has context about completed work
    defn = _make_defn(name="developer", role="developer")
    executor = _make_executor(api_client=api_client)

    # Provide conversation context via conductor snapshots
    class FakeConductor:
        _executor_snapshots: dict[str, list[dict]] = {}
    conductor = FakeConductor()
    conductor._executor_snapshots["test-task"] = [
        {"role": "user", "content": (
            "Fix the broken import in pkg/io.py. Change load_defaults to get_defaults."
        )},
        {"role": "assistant", "content": (
            "I've fixed pkg/io.py: changed the import from load_defaults to get_defaults. "
            "All tests in pkg/tests/test_io.py pass."
        )},
    ]
    executor.team_run.conductor = conductor

    # Use a fake task with the right id for snapshot lookup
    @dataclass
    class FakeTask:
        id: str = "test-task"
        agent_name: str = "developer"

    result = await executor._run_post_run(task=FakeTask(), defn=defn, ctx=ctx)

    assert isinstance(result, AgentResult)
    assert result.submitted_plan is None, "Developer should not submit a plan"
    assert result.submitted_replan is None, "Developer should not submit a replan"
    assert len(result.summary) > 10, f"Summary too short: {result.summary}"


# ---------------------------------------------------------------------------
# Test 6: Streaming runner — planner submit_plan (real LLM)
# ---------------------------------------------------------------------------


@pytest.mark.skipif(not HAS_CREDENTIALS, reason="No credentials")
@pytest.mark.asyncio
async def test_streaming_runner_planner_submit_plan(api_client):
    """No metadata submission → runner invokes LLM with planner posthook → submit_plan."""
    ctx = _make_ctx(role="planner", agent_name="team_planner")
    defn = _make_defn(name="team_planner", role="planner")
    executor = _make_executor(api_client=api_client)

    class FakeConductor:
        _executor_snapshots: dict[str, list[dict]] = {}
    conductor = FakeConductor()
    conductor._executor_snapshots["plan-task"] = [
        {"role": "user", "content": (
            "Decompose: Fix the authentication module. It has three files: "
            "src/auth/login.py, src/auth/session.py, src/auth/middleware.py. "
            "Each needs independent fixes. Use agent 'developer'."
        )},
        {"role": "assistant", "content": (
            "I've analyzed the auth module. Three independent concerns — "
            "login, session, middleware — can each be a separate developer task "
            "with no cross-dependencies."
        )},
    ]
    executor.team_run.conductor = conductor

    @dataclass
    class FakeTask:
        id: str = "plan-task"
        agent_name: str = "team_planner"

    result = await executor._run_post_run(task=FakeTask(), defn=defn, ctx=ctx)

    assert isinstance(result, AgentResult)
    assert result.submitted_plan is not None, "Planner should submit a plan"
    assert len(result.submitted_plan.tasks) >= 2, (
        f"Plan should have 2+ tasks, got {len(result.submitted_plan.tasks)}"
    )


# ---------------------------------------------------------------------------
# Test 7: Streaming runner — replanner declare_blocker (real LLM)
# ---------------------------------------------------------------------------


@pytest.mark.skipif(not HAS_CREDENTIALS, reason="No credentials")
@pytest.mark.asyncio
async def test_streaming_runner_replanner_declare_blocker(api_client):
    """No metadata → runner invokes LLM with replanner tools → declare_blocker."""
    ctx = _make_ctx(role="replanner", agent_name="team_replanner")
    defn = _make_defn(name="team_replanner", role="replanner")
    executor = _make_executor(api_client=api_client)

    class FakeConductor:
        _executor_snapshots: dict[str, list[dict]] = {}
    conductor = FakeConductor()
    conductor._executor_snapshots["replan-task"] = [
        {"role": "user", "content": (
            "A sibling task failed. Context:\n"
            "## Failed task\n"
            "**Task ID:** fix-io\n"
            "**Failure:** ImportError: cannot import 'load_defaults' from 'pkg._compat'\n\n"
            "## Sibling statuses\n"
            "- fix-compat [DONE]: Renamed load_defaults→get_defaults in pkg/_compat.py\n"
            "- fix-io [FAILED]: pkg/io.py line 3 imports load_defaults\n"
            "- fix-parser [RUNNING]: pkg/parser.py line 7 imports load_defaults\n"
            "- fix-cli [RUNNING]: pkg/cli.py line 2 imports load_defaults\n\n"
            "All running siblings import the renamed symbol. Shared break."
        )},
        {"role": "assistant", "content": (
            "This is clearly a shared dependency break — fix-compat renamed "
            "load_defaults and all importers will fail. I need to declare a blocker "
            "on pkg/_compat.py so running siblings are paused."
        )},
    ]
    executor.team_run.conductor = conductor

    @dataclass
    class FakeTask:
        id: str = "replan-task"
        agent_name: str = "team_replanner"

    result = await executor._run_post_run(task=FakeTask(), defn=defn, ctx=ctx)

    assert isinstance(result, BlockerDeclaration), (
        f"Expected BlockerDeclaration, got {type(result).__name__}: {result}"
    )
    assert len(result.root_cause_paths) >= 1
    assert len(result.reason) > 10


# ---------------------------------------------------------------------------
# Test 8: Streaming runner fallback — no api_client → legacy
# ---------------------------------------------------------------------------


@pytest.mark.skipif(not HAS_CREDENTIALS, reason="No credentials")
@pytest.mark.asyncio
async def test_no_api_client_falls_back_to_legacy():
    """When team_run has no api_client, _run_post_run returns legacy result."""
    ctx = _make_ctx(role="developer", work_result="I fixed the bug in io.py")
    defn = _make_defn()
    executor = _make_executor(api_client=None)  # no api_client

    result = await executor._run_post_run(task=None, defn=defn, ctx=ctx)

    # Should fall through to legacy which extracts work_result
    assert isinstance(result, AgentResult)
    assert "fixed the bug" in result.summary


# ---------------------------------------------------------------------------
# Test 9: Legacy path — planner with no submission → sentinel summary
# ---------------------------------------------------------------------------


@pytest.mark.skipif(not HAS_CREDENTIALS, reason="No credentials")
@pytest.mark.asyncio
async def test_planner_no_submission_no_api_client():
    """Planner that didn't submit and has no api_client → 'planner_did_not_submit_plan'."""
    ctx = _make_ctx(role="planner", agent_name="team_planner")
    defn = _make_defn(name="team_planner", role="planner")
    executor = _make_executor(api_client=None)

    result = await executor._run_post_run(task=None, defn=defn, ctx=ctx)

    # Legacy path for planner with no submission
    assert isinstance(result, AgentResult)
    assert result.summary == "planner_did_not_submit_plan"


# ---------------------------------------------------------------------------
# Test 10: Legacy path — developer with no submission → sentinel summary
# ---------------------------------------------------------------------------


@pytest.mark.skipif(not HAS_CREDENTIALS, reason="No credentials")
@pytest.mark.asyncio
async def test_developer_no_submission_no_api_client():
    """Developer that didn't submit and has no api_client → 'completed (no explicit submission)'."""
    ctx = _make_ctx(role="developer", agent_name="developer")
    defn = _make_defn()
    executor = _make_executor(api_client=None)

    result = await executor._run_post_run(task=None, defn=defn, ctx=ctx)

    assert isinstance(result, AgentResult)
    assert result.summary == "completed (no explicit submission)"


# ---------------------------------------------------------------------------
# Test 11: Legacy + streaming coexistence — ReplanPlan in metadata
# ---------------------------------------------------------------------------


@pytest.mark.skipif(not HAS_CREDENTIALS, reason="No credentials")
@pytest.mark.asyncio
async def test_legacy_honours_submitted_replan_plan():
    """ReplanPlan (from add_tasks/cancel_and_redraft) in metadata is honoured."""
    replan = ReplanPlan.from_dict({
        "add_tasks": [
            {"id": "retry-io", "task": "Retry IO fix", "agent": "developer", "deps": [], "scope_paths": ["pkg/io.py"]},
        ],
        "cancel_ids": ["fix-io"],
    })
    ctx = _make_ctx(role="replanner", submitted_output=replan)
    defn = _make_defn(name="team_replanner", role="replanner")
    executor = _make_executor()

    result = await executor._run_post_run(task=None, defn=defn, ctx=ctx)

    assert isinstance(result, AgentResult)
    assert result.submitted_replan is not None
    assert len(result.submitted_replan.add_tasks) == 1
    assert len(result.submitted_replan.cancel_ids) == 1
