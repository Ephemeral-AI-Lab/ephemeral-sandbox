"""End-to-end TeamRun tests with a scripted runner (no real LLM calls)."""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any

import pytest

from agents.registry import register_definition, unregister_definition
from agents.types import AgentDefinition
from hooks.agent_posthook import PosthookConfig
from team.run import TeamRun
from team.types import AgentResult, Plan, TeamRunStatus, WorkItemStatus
from team.worker import Worker


@dataclass
class ScriptedCtx:
    defn_name: str = ""
    tool_metadata: dict[str, Any] = field(default_factory=dict)


def _register_scripted(name: str, posthook: PosthookConfig | None = None) -> AgentDefinition:
    # Posthook serializer agents must not carry builtin skills (enforced
    # by hooks.agent_posthook). The scripted helper is reused for both
    # work agents and serializers, so we always opt out of skills here.
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


def make_runner(scripts: dict[str, Any]):
    """Return a QueryRunner that drives each agent per a script.

    ``scripts[agent_name]`` is either:
      - a dict with ``artifact`` / ``summary`` / ``plan`` → plain work agent
      - a callable(ctx) that mutates ctx.tool_metadata (for posthook phases)
    """

    async def runner(defn, ctx):
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


def extract_result(work_result, wi) -> AgentResult:
    if isinstance(work_result, dict) and "artifact" in work_result:
        return AgentResult(
            artifact=work_result["artifact"], summary=work_result.get("summary", "")
        )
    return AgentResult(artifact=work_result, summary=str(work_result)[:100])


def make_worker_factory(runner):
    def build_query_ctx(defn, team_run, wi):
        ctx = ScriptedCtx(defn_name=defn.name)
        ctx.tool_metadata["team_context"] = {
            "team_run_id": team_run.id,
            "work_item_id": wi.id,
            "agent_run_id": wi.agent_run_id,
        }
        return ctx

    def build_posthook_ctx(posthook_defn, work_result):
        return ScriptedCtx(defn_name=posthook_defn.name)

    def factory(team_run):
        from agents.registry import get_definition

        return Worker(
            team_run=team_run,
            runner=runner,
            build_query_context=build_query_ctx,
            build_posthook_context=build_posthook_ctx,
            extract_result=extract_result,
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
            worker_factory=make_worker_factory(make_runner(scripts)),
            num_workers=1,
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
            worker_factory=make_worker_factory(make_runner(scripts)),
            num_workers=2,
        )
        status = await tr.wait()
        assert status == TeamRunStatus.SUCCEEDED
        # Planner + 2 children = 3 WorkItems total
        assert len(tr.dispatcher.graph) == 3
        done = [wi for wi in tr.dispatcher.graph.values() if wi.status == WorkItemStatus.DONE]
        assert len(done) == 3
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
            worker_factory=make_worker_factory(make_runner(scripts)),
        )
        await tr.wait()
        root = tr.dispatcher.graph[tr.root_work_item_id]
        assert root.status == WorkItemStatus.FAILED
        assert "NoPosthookOutput" in (root.failure_reason or "")
    finally:
        _cleanup("stubborn_planner", "submit_plan_agent")


@pytest.mark.asyncio
async def test_sibling_view_reflects_live_completion_during_run():
    """A later-running WorkItem should see earlier siblings' results live."""
    _register_scripted("submit_plan_agent")
    _register_scripted("planner", posthook=PosthookConfig(agent_name="submit_plan_agent", metadata_key="submitted_plan"))
    _register_scripted("observer")
    _register_scripted("producer")
    observed: dict[str, Any] = {}

    try:

        def planner_posthook(ctx):
            ctx.tool_metadata["submitted_plan"] = Plan.from_dict(
                {
                    "items": [
                        {"agent_name": "producer", "local_id": "p"},
                        {"agent_name": "observer", "local_id": "o", "deps": ["p"]},
                    ]
                }
            )

        def observer_work(ctx):
            # mimic an observer that inspects live state via team_list_siblings
            # at execution time
            from team.context.files import get_active_team_run

            tctx = ctx.tool_metadata["team_context"]
            tr = get_active_team_run(tctx["team_run_id"])
            from team.context.siblings import SiblingView

            view = SiblingView(tr.dispatcher, tctx["work_item_id"], tr.artifacts)
            observed["siblings"] = [s["status"] for s in view.list()]
            return {"artifact": "observed", "summary": "seen"}

        async def runner(defn, ctx):
            if defn.name == "planner":
                return {"artifact": {}, "summary": ""}
            if defn.name == "submit_plan_agent":
                planner_posthook(ctx)
                return None
            if defn.name == "producer":
                return {"artifact": {"val": 42}, "summary": "produced"}
            if defn.name == "observer":
                return observer_work(ctx)
            return None

        def build_query_ctx(defn, team_run, wi):
            ctx = ScriptedCtx(defn_name=defn.name)
            ctx.tool_metadata["team_context"] = {
                "team_run_id": team_run.id,
                "work_item_id": wi.id,
                "agent_run_id": wi.agent_run_id,
            }
            return ctx

        def build_posthook_ctx(posthook_defn, work_result):
            return ScriptedCtx(defn_name=posthook_defn.name)

        def factory(team_run):
            from agents.registry import get_definition

            return Worker(
                team_run=team_run,
                runner=runner,
                build_query_context=build_query_ctx,
                build_posthook_context=build_posthook_ctx,
                extract_result=extract_result,
                agent_lookup=get_definition,
            )

        tr = TeamRun(session_id="S1", user_request="x")
        await tr.start("planner", payload={}, worker_factory=factory, num_workers=1)
        await tr.wait()
        # The observer ran after the producer completed, so it must see
        # producer as DONE (and the planner as DONE) in its live view.
        statuses = observed["siblings"]
        assert "done" in statuses  # producer is done
    finally:
        _cleanup("planner", "producer", "observer", "submit_plan_agent")


@pytest.mark.asyncio
async def test_checkpoint_rollback_cooperative_drain():
    _register_scripted("solo")
    try:
        scripts = {"solo": {"artifact": {"x": 1}, "summary": "done"}}
        tr = TeamRun(session_id="S1", user_request="x")
        await tr.start(
            "solo",
            payload={},
            worker_factory=make_worker_factory(make_runner(scripts)),
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


@pytest.mark.asyncio
async def test_sandbox_id_propagates_to_query_context_builder():
    """The build_query_context callback must be able to read team_run.sandbox_id
    so it can forward it into spawn_agent() for sandbox/code-intel toolkits."""
    _register_scripted("solo")
    try:
        captured: dict[str, Any] = {}

        def build_query_ctx(defn, team_run, wi):
            captured["sandbox_id"] = team_run.sandbox_id
            ctx = ScriptedCtx(defn_name=defn.name)
            ctx.tool_metadata["sandbox_id"] = team_run.sandbox_id or ""
            return ctx

        def build_posthook_ctx(posthook_defn, work_result):
            return ScriptedCtx(defn_name=posthook_defn.name)

        def factory(team_run):
            from agents.registry import get_definition

            return Worker(
                team_run=team_run,
                runner=make_runner({"solo": {"artifact": {}, "summary": "ok"}}),
                build_query_context=build_query_ctx,
                build_posthook_context=build_posthook_ctx,
                extract_result=extract_result,
                agent_lookup=get_definition,
            )

        tr = TeamRun(session_id="S1", user_request="hello", sandbox_id="sb-xyz")
        await tr.start("solo", payload={}, worker_factory=factory)
        status = await tr.wait()
        assert status == TeamRunStatus.SUCCEEDED
        assert captured["sandbox_id"] == "sb-xyz"
    finally:
        _cleanup("solo")
