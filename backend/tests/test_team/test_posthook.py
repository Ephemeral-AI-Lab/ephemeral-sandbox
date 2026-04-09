"""Unit tests for hooks.agent_posthook.execute_with_posthook + C3 toolkit restriction."""

from __future__ import annotations

from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

import pytest

from agents.types import AgentDefinition
from hooks.agent_posthook import (
    NoPosthookOutput,
    PosthookConfig,
    PosthookError,
    PosthookMisconfigured,
    execute_with_posthook,
)
from tools.posthook.toolkits import SubmitReplanPosthookToolkit, SubmitRetryPosthookToolkit


@dataclass
class FakeCtx:
    tool_metadata: dict[str, Any] = field(default_factory=dict)


def _make_defn(name: str = "work", posthook: PosthookConfig | None = None) -> AgentDefinition:
    return AgentDefinition(
        name=name,
        description="test",
        system_prompt="p",
        toolkits=["sandbox_operations"],
        posthook=posthook,
    )


def _make_serializer(name: str = "submit_plan_agent") -> AgentDefinition:
    return AgentDefinition(
        name=name,
        description="serializer",
        system_prompt="serialize",
        toolkits=["submit_plan_posthook"],
        include_skills=False,
        source="builtin",
    )


@pytest.mark.asyncio
async def test_no_posthook_returns_work_result():
    defn = _make_defn()
    ctx = FakeCtx()

    async def runner(d, c):
        return {"phase": "work"}

    result, submitted = await execute_with_posthook(defn, ctx, runner=runner)
    assert result == {"phase": "work"}
    assert submitted is None


@pytest.mark.asyncio
async def test_posthook_phase_runs_and_extracts_submission():
    cfg = PosthookConfig(agent_name="submit_plan_agent", metadata_key="submitted_plan")
    defn = _make_defn(posthook=cfg)
    serializer = _make_serializer()
    work_ctx = FakeCtx()

    call_log: list[str] = []

    async def runner(d, c):
        call_log.append(d.name)
        if d.name == "submit_plan_agent":
            c.tool_metadata["submitted_plan"] = {"items": [{"agent_name": "a"}]}
        return {"phase": d.name}

    def lookup(name):
        return serializer if name == "submit_plan_agent" else None

    def build_posthook_ctx(posthook_defn, work_result):
        assert posthook_defn.toolkits == ["submit_plan_posthook"]
        return FakeCtx()

    result, submitted = await execute_with_posthook(
        defn,
        work_ctx,
        runner=runner,
        agent_lookup=lookup,
        posthook_ctx_builder=build_posthook_ctx,
    )
    assert call_log == ["work", "submit_plan_agent"]
    assert submitted == {"items": [{"agent_name": "a"}]}


@pytest.mark.asyncio
async def test_posthook_raises_when_submission_missing():
    cfg = PosthookConfig(agent_name="submit_plan_agent", metadata_key="submitted_plan")
    defn = _make_defn(posthook=cfg)
    serializer = _make_serializer()

    async def runner(d, c):
        return None

    with pytest.raises(NoPosthookOutput):
        await execute_with_posthook(
            defn,
            FakeCtx(),
            runner=runner,
            agent_lookup=lambda n: serializer,
            posthook_ctx_builder=lambda d, r: FakeCtx(),
        )


@pytest.mark.asyncio
async def test_work_phase_already_submitted_skips_posthook():
    cfg = PosthookConfig(agent_name="submit_plan_agent", metadata_key="submitted_plan")
    defn = _make_defn(posthook=cfg)
    ctx = FakeCtx()
    calls: list[str] = []

    async def runner(d, c):
        calls.append(d.name)
        ctx.tool_metadata["submitted_plan"] = {"items": [{"agent_name": "x"}]}
        return "work"

    result, submitted = await execute_with_posthook(
        defn,
        ctx,
        runner=runner,
        agent_lookup=lambda n: _make_serializer(),
        posthook_ctx_builder=lambda d, r: FakeCtx(),
    )
    assert calls == ["work"]
    assert submitted == {"items": [{"agent_name": "x"}]}


# ---- Eager validation: misconfigurations fail BEFORE the work phase runs ----


@pytest.mark.asyncio
async def test_missing_agent_lookup_raises_before_work_runs():
    cfg = PosthookConfig(agent_name="submit_plan_agent")
    defn = _make_defn(posthook=cfg)
    work_called = False

    async def runner(d, c):
        nonlocal work_called
        work_called = True
        return "work"

    with pytest.raises(PosthookMisconfigured, match="agent_lookup or posthook_ctx_builder"):
        await execute_with_posthook(defn, FakeCtx(), runner=runner)

    assert work_called is False, "work phase must not run when posthook deps are missing"


@pytest.mark.asyncio
async def test_missing_ctx_builder_raises_before_work_runs():
    cfg = PosthookConfig(agent_name="submit_plan_agent")
    defn = _make_defn(posthook=cfg)
    work_called = False

    async def runner(d, c):
        nonlocal work_called
        work_called = True
        return "work"

    with pytest.raises(PosthookMisconfigured, match="agent_lookup or posthook_ctx_builder"):
        await execute_with_posthook(
            defn, FakeCtx(), runner=runner, agent_lookup=lambda n: _make_serializer()
        )

    assert work_called is False


@pytest.mark.asyncio
async def test_unregistered_serializer_raises_before_work_runs():
    cfg = PosthookConfig(agent_name="ghost")
    defn = _make_defn(posthook=cfg)
    work_called = False

    async def runner(d, c):
        nonlocal work_called
        work_called = True
        return "work"

    with pytest.raises(PosthookMisconfigured, match="not registered"):
        await execute_with_posthook(
            defn,
            FakeCtx(),
            runner=runner,
            agent_lookup=lambda n: None,
            posthook_ctx_builder=lambda d, r: FakeCtx(),
        )

    assert work_called is False


@pytest.mark.asyncio
async def test_misconfigured_subclasses_posthook_error():
    """PosthookMisconfigured must be catchable as PosthookError."""
    cfg = PosthookConfig(agent_name="ghost")
    defn = _make_defn(posthook=cfg)

    async def runner(d, c):
        return "work"

    with pytest.raises(PosthookError):
        await execute_with_posthook(
            defn,
            FakeCtx(),
            runner=runner,
            agent_lookup=lambda n: None,
            posthook_ctx_builder=lambda d, r: FakeCtx(),
        )


@pytest.mark.asyncio
async def test_no_output_subclasses_posthook_error():
    """NoPosthookOutput must be catchable as PosthookError."""
    cfg = PosthookConfig(agent_name="submit_plan_agent", metadata_key="submitted_plan")
    defn = _make_defn(posthook=cfg)

    async def runner(d, c):
        return None

    with pytest.raises(PosthookError):
        await execute_with_posthook(
            defn,
            FakeCtx(),
            runner=runner,
            agent_lookup=lambda n: _make_serializer(),
            posthook_ctx_builder=lambda d, r: FakeCtx(),
        )


# ---- No-skills contract: pure submit serializers must not carry builtin skills ----


@pytest.mark.asyncio
async def test_serializer_with_include_skills_true_is_rejected():
    cfg = PosthookConfig(agent_name="bad_serializer", metadata_key="submitted_plan")
    defn = _make_defn(posthook=cfg)
    bad = AgentDefinition(
        name="bad_serializer",
        description="bad",
        system_prompt="p",
        toolkits=["submit_plan_posthook"],
        include_skills=True,  # violates contract
        skills=[],
        source="builtin",
    )

    async def runner(d, c):
        return "work"

    with pytest.raises(PosthookMisconfigured, match="pure submit posthook agent .* must not be equipped with builtin skills"):
        await execute_with_posthook(
            defn,
            FakeCtx(),
            runner=runner,
            agent_lookup=lambda n: bad,
            posthook_ctx_builder=lambda d, r: FakeCtx(),
        )


def test_decision_posthook_toolkits_allow_summary_retry_and_replan() -> None:
    retry_tools = set(SubmitRetryPosthookToolkit().tool_names())
    replan_tools = set(SubmitReplanPosthookToolkit().tool_names())

    assert retry_tools == {"submit_summary", "request_retry", "request_replan"}
    assert replan_tools == {"submit_summary", "request_retry", "request_replan"}


@pytest.mark.asyncio
async def test_serializer_with_nonempty_skills_is_rejected():
    cfg = PosthookConfig(agent_name="bad_serializer", metadata_key="submitted_plan")
    defn = _make_defn(posthook=cfg)
    bad = AgentDefinition(
        name="bad_serializer",
        description="bad",
        system_prompt="p",
        toolkits=["submit_plan_posthook"],
        include_skills=False,
        skills=["some_skill"],  # violates contract
        source="builtin",
    )

    async def runner(d, c):
        return "work"

    with pytest.raises(PosthookMisconfigured, match="pure submit posthook agent .* must not be equipped with builtin skills"):
        await execute_with_posthook(
            defn,
            FakeCtx(),
            runner=runner,
            agent_lookup=lambda n: bad,
            posthook_ctx_builder=lambda d, r: FakeCtx(),
        )


@pytest.mark.asyncio
async def test_serializer_with_no_skills_is_accepted():
    cfg = PosthookConfig(agent_name="ok_serializer", metadata_key="submitted_plan")
    defn = _make_defn(posthook=cfg)
    ok = _make_serializer("ok_serializer")  # include_skills=False, skills=[]

    async def runner(d, c):
        if d.name == "ok_serializer":
            c.tool_metadata["submitted_plan"] = {"ok": True}
        return d.name

    _, submitted = await execute_with_posthook(
        defn,
        FakeCtx(),
        runner=runner,
        agent_lookup=lambda n: ok,
        posthook_ctx_builder=lambda d, r: FakeCtx(),
    )
    assert submitted == {"ok": True}


@pytest.mark.asyncio
async def test_decision_posthook_with_skills_is_accepted() -> None:
    cfg = PosthookConfig(agent_name="decision_submit_retry", metadata_key="submitted_summary")
    defn = _make_defn(posthook=cfg)
    decision = AgentDefinition(
        name="decision_submit_retry",
        description="decision",
        system_prompt="p",
        toolkits=["posthook_submit_retry"],
        include_skills=True,
        skills=["team-posthook-decision-playbook"],
        source="builtin",
    )

    async def runner(d, c):
        if d.name == "decision_submit_retry":
            c.tool_metadata["submitted_summary"] = {"ok": True}
        return d.name

    _, submitted = await execute_with_posthook(
        defn,
        FakeCtx(),
        runner=runner,
        agent_lookup=lambda n: decision,
        posthook_ctx_builder=lambda d, r: FakeCtx(),
    )
    assert submitted == {"ok": True}


@pytest.mark.asyncio
async def test_decision_named_posthook_with_submit_toolkit_and_skills_is_accepted() -> None:
    cfg = PosthookConfig(agent_name="decision_submit_retry", metadata_key="submitted_summary")
    defn = _make_defn(posthook=cfg)
    decision = AgentDefinition(
        name="decision_submit_retry",
        description="decision",
        system_prompt="p",
        toolkits=["submit_summary_posthook"],
        include_skills=True,
        skills=["team-posthook-decision-playbook"],
        source="builtin",
    )

    async def runner(d, c):
        if d.name == "decision_submit_retry":
            c.tool_metadata["submitted_summary"] = {"ok": True}
        return d.name

    _, submitted = await execute_with_posthook(
        defn,
        FakeCtx(),
        runner=runner,
        agent_lookup=lambda n: decision,
        posthook_ctx_builder=lambda d, r: FakeCtx(),
    )
    assert submitted == {"ok": True}


# ---- Metadata-key plumbing ----


@pytest.mark.asyncio
async def test_metadata_key_is_stamped_on_work_ctx_before_runner():
    """The submit tool reads `posthook_metadata_key` from ctx.tool_metadata
    to know which slot to write into. The helper must stamp this BEFORE
    the work phase runs, so a work agent that calls the submit tool
    directly can still discover the right slot."""
    cfg = PosthookConfig(agent_name="ok_serializer", metadata_key="custom_slot")
    defn = _make_defn(posthook=cfg)
    work_ctx = FakeCtx()
    seen_key: dict[str, Any] = {}

    async def runner(d, c):
        if d.name == "work":
            seen_key["work"] = c.tool_metadata.get("posthook_metadata_key")
        else:
            seen_key["serializer"] = c.tool_metadata.get("posthook_metadata_key")
            c.tool_metadata["custom_slot"] = {"v": 1}
        return d.name

    await execute_with_posthook(
        defn,
        work_ctx,
        runner=runner,
        agent_lookup=lambda n: _make_serializer("ok_serializer"),
        posthook_ctx_builder=lambda d, r: FakeCtx(),
    )
    assert seen_key["work"] == "custom_slot"
    assert seen_key["serializer"] == "custom_slot"


@pytest.mark.asyncio
async def test_metadata_key_not_stamped_when_no_posthook():
    """No posthook configured → no key stamped, no surprise mutation."""
    defn = _make_defn()  # no posthook
    ctx = FakeCtx()

    async def runner(d, c):
        return "work"

    await execute_with_posthook(defn, ctx, runner=runner)
    assert "posthook_metadata_key" not in ctx.tool_metadata


# ---- Posthook ctx builder receives the work result ----


@pytest.mark.asyncio
async def test_posthook_ctx_builder_receives_work_result():
    cfg = PosthookConfig(agent_name="ok_serializer", metadata_key="submitted_plan")
    defn = _make_defn(posthook=cfg)
    captured: dict[str, Any] = {}

    async def runner(d, c):
        if d.name == "ok_serializer":
            c.tool_metadata["submitted_plan"] = "done"
        return {"phase": d.name, "value": 42}

    def builder(posthook_defn, work_result):
        captured["defn_name"] = posthook_defn.name
        captured["work_result"] = work_result
        return FakeCtx()

    await execute_with_posthook(
        defn,
        FakeCtx(),
        runner=runner,
        agent_lookup=lambda n: _make_serializer("ok_serializer"),
        posthook_ctx_builder=builder,
    )
    assert captured["defn_name"] == "ok_serializer"
    assert captured["work_result"] == {"phase": "work", "value": 42}


# ---- Logging on the "work already submitted" short-circuit ----


@pytest.mark.asyncio
async def test_already_submitted_branch_logs_debug(caplog):
    import logging

    cfg = PosthookConfig(agent_name="ok_serializer", metadata_key="submitted_plan")
    defn = _make_defn(posthook=cfg)
    ctx = FakeCtx()

    async def runner(d, c):
        ctx.tool_metadata["submitted_plan"] = {"early": True}
        return "work"

    with caplog.at_level(logging.DEBUG, logger="hooks.agent_posthook"):
        _, submitted = await execute_with_posthook(
            defn,
            ctx,
            runner=runner,
            agent_lookup=lambda n: _make_serializer("ok_serializer"),
            posthook_ctx_builder=lambda d, r: FakeCtx(),
        )

    assert submitted == {"early": True}
    assert any("already submitted" in rec.message for rec in caplog.records)


# ---- Work phase exceptions propagate untouched ----


@pytest.mark.asyncio
async def test_work_phase_exception_propagates():
    cfg = PosthookConfig(agent_name="ok_serializer", metadata_key="submitted_plan")
    defn = _make_defn(posthook=cfg)

    class Boom(RuntimeError):
        pass

    async def runner(d, c):
        raise Boom("work blew up")

    with pytest.raises(Boom):
        await execute_with_posthook(
            defn,
            FakeCtx(),
            runner=runner,
            agent_lookup=lambda n: _make_serializer("ok_serializer"),
            posthook_ctx_builder=lambda d, r: FakeCtx(),
        )


# ---- C3: posthook agent's tool registry contains exactly {submit_plan} ----


def test_submit_plan_agent_registry_strictly_contains_only_submit_tool():
    """The serializer agent's spawning registry must expose exactly one tool."""
    from team.builtins import register_all
    from engine.runtime.agent import _build_agent_tool_registry

    register_all()  # idempotent

    from agents.registry import get_definition

    serializer = get_definition("submit_plan_agent")
    assert serializer is not None

    class _Cfg:
        cwd = str(Path.cwd())

    registry = _build_agent_tool_registry(
        _Cfg(), serializer, sandbox_id=None, agent_name="submit_plan_agent"
    )
    tool_names = {t.name for t in registry.list_tools()}
    assert tool_names == {"submit_plan"}, f"expected only submit_plan, got {tool_names}"


def test_team_planner_definition_uses_submit_plan_posthook_not_submit_toolkit():
    from team.builtins import SUBMIT_PLAN_AGENT, TEAM_PLANNER, register_all
    from agents.registry import get_definition

    register_all()  # idempotent

    planner = get_definition(TEAM_PLANNER)
    assert planner is not None
    assert planner.posthook is not None
    assert planner.posthook.agent_name == SUBMIT_PLAN_AGENT
    assert "submit_plan_posthook" not in planner.toolkits
    assert "submit_replan_posthook" not in planner.toolkits
    assert "posthook_submit_replan" not in planner.toolkits


def test_team_replanner_definition_uses_submit_replan_posthook_not_replan_tools():
    from team.builtins import SUBMIT_REPLAN_AGENT, TEAM_REPLANNER, register_all
    from agents.registry import get_definition

    register_all()  # idempotent

    replanner = get_definition(TEAM_REPLANNER)
    assert replanner is not None
    assert replanner.posthook is not None
    assert replanner.posthook.agent_name == SUBMIT_REPLAN_AGENT
    assert "submit_plan_posthook" not in replanner.toolkits
    assert "submit_replan_posthook" not in replanner.toolkits
    assert "posthook_submit_replan" not in replanner.toolkits
    assert "replan_operations" not in replanner.toolkits


def test_submit_replan_agent_definition_uses_only_replan_submit_toolkit():
    from team.builtins import SUBMIT_REPLAN_AGENT, register_all
    from agents.registry import get_definition

    register_all()  # idempotent

    serializer = get_definition(SUBMIT_REPLAN_AGENT)
    assert serializer is not None
    assert serializer.toolkits == ["submit_replan_posthook"]
    assert serializer.include_skills is False
    assert serializer.skills == []
