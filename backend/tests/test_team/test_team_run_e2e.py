"""End-to-end TeamRun tests with a scripted runner (no real LLM calls)."""

from __future__ import annotations

from typing import Any

import pytest

from agents.registry import register_definition, unregister_definition
from agents.types import AgentDefinition
from hooks.agent_posthook import PosthookConfig
from team.artifacts.store import InMemoryArtifactStore
from team.context.project import ProjectContext
from team.models import BudgetConfig, BudgetState, Plan, TeamRunStatus, WorkItemKind, WorkItemStatus
from team.runtime.context_builder import TeamAgentContext
from team.runtime.dispatcher import Dispatcher
from team.runtime.team_run import TeamRun
from team.runtime.executor import Executor
from team.runtime.team_run import TeamRuntimeServices
from tools.posthook import SubmittedSummary

pytestmark = pytest.mark.e2e

def _register_scripted(name: str, posthook: PosthookConfig | None = None) -> AgentDefinition:
    # Posthook serializer agents must not carry builtin skills (enforced
    # by hooks.agent_posthook). The scripted helper is reused for both
    # work agents and serializers, so we always opt out of skills here.
    #
    # Runtime invariant (Step 2d): every team agent must have a posthook.
    # If the test does not supply one, auto-wire a per-agent ``submit_summary``
    # serializer (``f"{name}__autopost"``) and register it alongside.
    if posthook is None:
        autopost_name = f"{name}__autopost"
        register_definition(
            AgentDefinition(
                name=autopost_name,
                description=f"autopost serializer for {name}",
                system_prompt="p",
                toolkits=[],
                skills=[],
                include_skills=False,
                source="builtin",
            )
        )
        posthook = PosthookConfig(
            agent_name=autopost_name, metadata_key="submitted_summary"
        )
    defn = AgentDefinition(
        name=name,
        description=f"scripted {name}",
        system_prompt="p",
        toolkits=[],
        skills=[],
        include_skills=False,
        posthook=posthook,
        source="builtin",
    )
    register_definition(defn)
    return defn


def _cleanup(*names: str) -> None:
    for n in names:
        unregister_definition(n)
        # Also unregister the auto-wired serializer if it exists.
        try:
            unregister_definition(f"{n}__autopost")
        except Exception:
            pass


def make_runner(scripts: dict[str, Any]):
    """Return a QueryRunner that drives each agent per a script.

    ``scripts[agent_name]`` is either:
      - a dict with ``artifact`` / ``summary`` / ``plan`` → plain work agent
      - a callable(ctx) that mutates ctx.tool_metadata (for posthook phases)
    """

    async def runner(defn, ctx):
        # Auto-posthook serializer for the default ``__autopost`` flow:
        # look up the work agent's script and stash a SubmittedSummary so
        # the Executor's posthook-required invariant is satisfied without
        # the test having to write a serializer by hand.
        if defn.name.endswith("__autopost"):
            owner = defn.name[: -len("__autopost")]
            owner_script = scripts.get(owner) or {}
            ctx.tool_metadata["submitted_summary"] = SubmittedSummary(
                summary=owner_script.get("summary", ""),
                artifact=owner_script.get("artifact"),
            )
            return {"phase": defn.name}

        script = scripts.get(defn.name)
        if callable(script):
            script(ctx)
            return {"phase": defn.name}
        if isinstance(script, dict):
            if "plan" in script:
                # work phase of a planner in non-posthook mode: stash submitted_plan on ctx
                ctx.tool_metadata["submitted_plan"] = script["plan"]
            return {
                "artifact": script.get("artifact", {}),
                "summary": script.get("summary", ""),
            }
        return {"artifact": {}, "summary": ""}

    return runner


def make_executor_factory(runner):
    def build_query_ctx(defn, team_run, wi):
        return TeamAgentContext(
            tool_metadata={
                "team_run_id": team_run.id,
                "work_item_id": wi.id,
                "agent_run_id": wi.agent_run_id,
                "agent_name": defn.name,
            }
        )

    def build_posthook_ctx(posthook_defn, work_result):
        return TeamAgentContext(
            tool_metadata={
                "agent_name": posthook_defn.name,
                "work_result": work_result,
            }
        )

    def factory(team_run):
        from agents.registry import get_definition

        return Executor(
            team_run=team_run,
            runner=runner,
            build_query_context=build_query_ctx,
            build_posthook_context=build_posthook_ctx,
            agent_lookup=get_definition,
        )

    return factory


# ---------------------------------------------------------------------------
# Tests
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_single_work_item_team_run():
    _register_scripted("solo")
    try:
        scripts = {"solo": {"artifact": {"x": 1}, "summary": "done"}}
        tr = TeamRun(session_id="S1", user_request="hello")
        await tr.start(
            "solo",
            payload={"task": "do-it"},
            executor_factory=make_executor_factory(make_runner(scripts)),
            num_executors=1,
        )
        status = await tr.wait()
        assert status == TeamRunStatus.SUCCEEDED
        root = tr.dispatcher.graph[tr.root_work_item_id]
        assert root.status == WorkItemStatus.DONE
        assert root.agent_run_id is not None
    finally:
        _cleanup("solo")


@pytest.mark.asyncio
async def test_planner_emits_plan_via_posthook_and_children_run():
    posthook = PosthookConfig(agent_name="submit_plan_agent", metadata_key="submitted_plan")
    _register_scripted("submit_plan_agent")
    _register_scripted("planner", posthook=posthook)
    _register_scripted("child")
    try:
        child_plan = Plan.from_dict(
            {
                "items": [
                    {"agent_name": "child", "local_id": "c1", "payload": {"n": 1}},
                    {
                        "agent_name": "child",
                        "local_id": "c2",
                        "payload": {"n": 2},
                        "deps": ["c1"],
                    },
                ]
            }
        )

        def planner_posthook_phase(ctx):
            ctx.tool_metadata["submitted_plan"] = child_plan

        scripts = {
            "planner": {"artifact": {"think": True}, "summary": "planned"},
            "submit_plan_agent": planner_posthook_phase,
            "child": {"artifact": {"out": "ok"}, "summary": "child-done"},
        }
        tr = TeamRun(session_id="S1", user_request="decompose")
        await tr.start(
            "planner",
            payload={},
            executor_factory=make_executor_factory(make_runner(scripts)),
            num_executors=2,
            root_kind=WorkItemKind.EXPANDABLE,
        )
        status = await tr.wait()
        assert status == TeamRunStatus.SUCCEEDED
        # Planner + 2 children = 3 WorkItems total
        assert len(tr.dispatcher.graph) == 3
        done = [wi for wi in tr.dispatcher.graph.values() if wi.status == WorkItemStatus.DONE]
        assert len(done) == 3
        planner_cps = [
            cp for cp in tr.dispatcher.list_checkpoints()
            if (cp.label or "").startswith("durable:complete:planner:")
        ]
        assert planner_cps
        planner_cp = planner_cps[-1]
        assert len(planner_cp.work_items) == 3
        local_ids = {
            wi.local_id
            for wi in planner_cp.work_items.values()
            if wi.local_id is not None
        }
        assert {"c1", "c2"} <= local_ids
    finally:
        _cleanup("planner", "child", "submit_plan_agent")


@pytest.mark.asyncio
async def test_planner_no_posthook_submission_fails_work_item():
    posthook = PosthookConfig(agent_name="submit_plan_agent", metadata_key="submitted_plan")
    _register_scripted("submit_plan_agent")
    _register_scripted("stubborn_planner", posthook=posthook)
    try:
        scripts = {
            "stubborn_planner": {"artifact": {}, "summary": "work"},
            # serializer DOES NOT set submitted_plan → NoPosthookOutput
            "submit_plan_agent": lambda ctx: None,
        }
        tr = TeamRun(session_id="S1", user_request="x")
        await tr.start(
            "stubborn_planner",
            payload={},
            executor_factory=make_executor_factory(make_runner(scripts)),
        )
        await tr.wait()
        root = tr.dispatcher.graph[tr.root_work_item_id]
        assert root.status == WorkItemStatus.FAILED
        assert "NoPosthookOutput" in (root.failure_reason or "")
    finally:
        _cleanup("stubborn_planner", "submit_plan_agent")


@pytest.mark.asyncio
async def test_checkpoint_rollback_cooperative_drain():
    _register_scripted("solo")
    try:
        scripts = {"solo": {"artifact": {"x": 1}, "summary": "done"}}
        tr = TeamRun(session_id="S1", user_request="x")
        await tr.start(
            "solo",
            payload={},
            executor_factory=make_executor_factory(make_runner(scripts)),
        )
        cp_id = await tr.checkpoint(label="pre-run")
        await tr.wait()
        assert tr.dispatcher.graph[tr.root_work_item_id].status == WorkItemStatus.DONE

        # Now rollback — root should revert to READY (its state when the cp was taken)
        await tr.rollback_to(cp_id)
        assert tr.dispatcher.graph[tr.root_work_item_id].status == WorkItemStatus.READY
    finally:
        _cleanup("solo")


def test_team_run_sandbox_id_defaults_to_none():
    tr = TeamRun(session_id="S1", user_request="hello")
    assert tr.sandbox_id is None


def test_team_run_sandbox_id_stored():
    tr = TeamRun(session_id="S1", user_request="hello", sandbox_id="sb-abc123")
    assert tr.sandbox_id == "sb-abc123"


def test_team_run_accepts_injected_runtime_services():
    budget_config = BudgetConfig()
    budget_state = BudgetState()
    project_context = ProjectContext(goal="g", user_request="u")
    artifact_store = InMemoryArtifactStore(budget_config, budget_state)
    dispatcher = Dispatcher(
        team_run_id="custom-run",
        budgets=budget_config,
        budget_state=budget_state,
        artifact_store=artifact_store,
    )
    services = TeamRuntimeServices(
        project_context=project_context,
        artifact_store=artifact_store,
        dispatcher=dispatcher,
    )

    tr = TeamRun(
        session_id="S1",
        user_request="hello",
        budgets=budget_config,
        services=services,
    )

    assert tr.project_context is project_context
    assert tr.artifacts is artifact_store
    assert tr.dispatcher is dispatcher


@pytest.mark.asyncio
async def test_sandbox_id_propagates_to_query_context_builder():
    """The build_query_context callback must be able to read team_run.sandbox_id
    so it can forward it into spawn_agent() for sandbox/code-intel toolkits."""
    _register_scripted("solo")
    try:
        captured: dict[str, Any] = {}

        def build_query_ctx(defn, team_run, wi):
            captured["sandbox_id"] = team_run.sandbox_id
            return TeamAgentContext(
                tool_metadata={
                    "agent_name": defn.name,
                    "sandbox_id": team_run.sandbox_id or "",
                }
            )

        def build_posthook_ctx(posthook_defn, work_result):
            return TeamAgentContext(
                tool_metadata={
                    "agent_name": posthook_defn.name,
                    "work_result": work_result,
                }
            )

        def factory(team_run):
            from agents.registry import get_definition

            return Executor(
                team_run=team_run,
                runner=make_runner({"solo": {"artifact": {}, "summary": "ok"}}),
                build_query_context=build_query_ctx,
                build_posthook_context=build_posthook_ctx,
                    agent_lookup=get_definition,
            )

        tr = TeamRun(session_id="S1", user_request="hello", sandbox_id="sb-xyz")
        await tr.start("solo", payload={}, executor_factory=factory)
        status = await tr.wait()
        assert status == TeamRunStatus.SUCCEEDED
        assert captured["sandbox_id"] == "sb-xyz"
    finally:
        _cleanup("solo")
