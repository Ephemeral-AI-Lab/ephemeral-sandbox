"""Step 4 — named subagent dispatch + typed envelope + shared briefings.

These tests live separately from the legacy ``test_run_subagent.py`` so the
new behaviours have a focused, easy-to-read home.
"""

from __future__ import annotations

import json
from pathlib import Path
from types import SimpleNamespace
from typing import Any

import pytest

from agents.registry import register_definition, unregister_definition
from agents import get_definition as _get_agent_def
from team.builtins import register_all as _register_team_builtins

# Builtins (including the ``scout`` agent) are registered lazily by
# ``__main__``. Tests bypass that entrypoint, so opt-in here — but be
# tolerant of double-registration when other tests already triggered it.
if _get_agent_def("scout") is None:
    try:
        _register_team_builtins()
    except Exception:
        pass
from agents.types import AgentDefinition
from engine.runtime.background_tasks import BackgroundTaskManager
from hooks.agent_posthook import PosthookConfig
from team.context.project import ProjectContext
from team.models import Briefing, BudgetConfig, BudgetState, Plan, WorkItemSpec
from team.runtime.registry import register as _register_team_run
from team.runtime.registry import unregister as _unregister_team_run
from team.artifacts.store import InMemoryArtifactStore
from tools.core.base import ToolExecutionContext
from tools.core.runtime import ExecutionMetadata
from tools.posthook import SubmittedSummary
from tools.subagent.run_subagent_tool import run_subagent


# ---------- shared test scaffolding ------------------------------------------


class _StubConfig:
    cwd = Path("/tmp")
    session_id = "S1"


def _make_stub_agent(submitted: Any | None = None, final_text: str = "ok") -> Any:
    """Mock the spawn_agent return so run_subagent can drive its lifecycle."""
    qc = SimpleNamespace(tool_metadata=ExecutionMetadata(), api_messages_snapshot=None)
    captured: dict[str, Any] = {}

    class _Stub:
        def __init__(self) -> None:
            self.display_messages: list[Any] = []
            self.query_context = qc

        async def run(self, prompt: str):  # type: ignore[no-untyped-def]
            captured["prompt"] = prompt
            from message.messages import ConversationMessage, TextBlock

            if submitted is not None:
                key = qc.tool_metadata.get("posthook_metadata_key", "submitted_output")
                qc.tool_metadata[key] = submitted
            self.display_messages.append(
                ConversationMessage(role="assistant", content=[TextBlock(text=final_text)])
            )
            if False:  # pragma: no cover - generator stub
                yield None
            return

    stub = _Stub()
    return stub, captured


def _ctx(*, team_run_id: str | None = None) -> ToolExecutionContext:
    bg = BackgroundTaskManager()

    async def _noop():
        from tools.core.base import ToolResult

        return ToolResult(output="placeholder")

    bg.launch(
        task_id="bg1",
        tool_name="run_subagent",
        tool_input={"prompt": "task"},
        coro=_noop(),
        task_note="test",
    )
    meta = ExecutionMetadata(
        session_config=_StubConfig(),
        background_task_manager=bg,
        background_task_id="bg1",
    )
    if team_run_id:
        meta.team_run_id = team_run_id
    return ToolExecutionContext(cwd=Path("/tmp"), metadata=meta)


def _patch_spawn(monkeypatch, stub) -> None:
    monkeypatch.setattr(
        "engine.runtime.agent.spawn_agent", lambda *a, **k: stub, raising=True
    )


# ---------- agent_name + recursion prevention --------------------------------


@pytest.mark.asyncio
async def test_unknown_agent_name_rejected(monkeypatch):
    res = await run_subagent.execute(
        run_subagent.input_model(agent_name="ghost", prompt="x"), _ctx()
    )
    assert res.is_error
    assert "not registered" in res.output


@pytest.mark.asyncio
async def test_non_subagent_target_rejected(monkeypatch):
    register_definition(AgentDefinition(name="not_a_subagent", description="d"))
    try:
        res = await run_subagent.execute(
            run_subagent.input_model(agent_name="not_a_subagent", prompt="x"), _ctx()
        )
        assert res.is_error
        assert "not a subagent" in res.output
    finally:
        unregister_definition("not_a_subagent")


# ---------- agent_name is mandatory ------------------------------------------


def test_agent_name_is_required_at_schema_level():
    """Pydantic must reject input that omits agent_name (no default)."""
    from pydantic import ValidationError

    with pytest.raises(ValidationError, match="agent_name"):
        run_subagent.input_model(prompt="x")


# ---------- prompt / input XOR -----------------------------------------------


@pytest.mark.asyncio
async def test_rejects_when_neither_prompt_nor_input():
    res = await run_subagent.execute(
        run_subagent.input_model(agent_name="subagent"), _ctx()
    )
    assert res.is_error
    assert "exactly one" in res.output


@pytest.mark.asyncio
async def test_rejects_when_both_prompt_and_input():
    res = await run_subagent.execute(
        run_subagent.input_model(agent_name="subagent", prompt="x", input={"y": 1}), _ctx()
    )
    assert res.is_error
    assert "exactly one" in res.output


@pytest.mark.asyncio
async def test_scout_rejects_prompt_mode():
    res = await run_subagent.execute(
        run_subagent.input_model(agent_name="scout", prompt="run the failing test"),
        _ctx(),
    )
    assert res.is_error
    assert "scout requires structured" in res.output


@pytest.mark.asyncio
async def test_scout_rejects_missing_target_paths():
    res = await run_subagent.execute(
        run_subagent.input_model(agent_name="scout", input={"task": "explore"}),
        _ctx(),
    )
    assert res.is_error
    assert "requires non-empty" in res.output


@pytest.mark.asyncio
async def test_scout_allows_seed_read_then_structural_exploration(monkeypatch):
    submitted = SubmittedSummary(
        summary="scout report",
        artifact={"target_paths": ["/testbed/pydantic/json_schema.py"], "files": []},
    )
    stub, _ = _make_stub_agent(submitted=submitted)
    _patch_spawn(monkeypatch, stub)

    ctx = _ctx()
    ctx.metadata["_read_paths_this_turn"] = ["/testbed/pydantic/json_schema.py"]

    res = await run_subagent.execute(
        run_subagent.input_model(
            agent_name="scout",
            input={"target_paths": ["/testbed/pydantic/json_schema.py"]},
        ),
        ctx,
    )

    assert not res.is_error


@pytest.mark.asyncio
async def test_scout_rejects_duplicate_exact_prior_scout_coverage(monkeypatch):
    ctx = _ctx()
    ctx.metadata["_scout_target_paths_this_turn"] = ["/testbed/pydantic/json_schema.py"]

    res = await run_subagent.execute(
        run_subagent.input_model(
            agent_name="scout",
            input={"target_paths": ["/testbed/pydantic/json_schema.py"]},
        ),
        ctx,
    )

    assert res.is_error
    assert "already covered in this turn" in res.output
    assert "submit the plan" in res.output


@pytest.mark.asyncio
async def test_scout_allows_duplicate_paths_for_atlas_refresh(monkeypatch):
    submitted = SubmittedSummary(
        summary="atlas scout report",
        artifact={"target_paths": ["/testbed/pydantic"], "files": []},
    )
    stub, _ = _make_stub_agent(submitted=submitted)
    _patch_spawn(monkeypatch, stub)

    ctx = _ctx()
    ctx.metadata["agent_name"] = "atlas_refresher"
    ctx.metadata["_read_paths_this_turn"] = ["/testbed/pydantic"]

    res = await run_subagent.execute(
        run_subagent.input_model(
            agent_name="scout",
            input={"target_paths": ["/testbed/pydantic"]},
        ),
        ctx,
    )

    assert not res.is_error


# ---------- typed envelope ----------------------------------------------------


@pytest.mark.asyncio
async def test_envelope_kind_brief_when_artifact_has_target_paths(monkeypatch):
    submitted = SubmittedSummary(
        summary="scout report",
        artifact={"target_paths": ["src/auth"], "files": []},
    )
    stub, _ = _make_stub_agent(submitted=submitted)
    _patch_spawn(monkeypatch, stub)

    res = await run_subagent.execute(
        run_subagent.input_model(agent_name="scout", input={"target_paths": ["src/auth"]}),
        _ctx(),
    )
    assert not res.is_error
    env = json.loads(res.output)
    assert env["kind"] == "brief"
    assert env["summary"] == "scout report"
    assert env["payload"]["target_paths"] == ["src/auth"]


@pytest.mark.asyncio
async def test_envelope_kind_summary_when_no_target_paths(monkeypatch):
    submitted = SubmittedSummary(summary="done", artifact={"files": ["a.py"]})
    stub, _ = _make_stub_agent(submitted=submitted)
    _patch_spawn(monkeypatch, stub)

    res = await run_subagent.execute(
        run_subagent.input_model(agent_name="subagent", prompt="do it"), _ctx()
    )
    assert not res.is_error
    env = json.loads(res.output)
    assert env["kind"] == "summary"
    assert env["payload"] == {"files": ["a.py"]}


@pytest.mark.asyncio
async def test_envelope_kind_plan(monkeypatch):
    plan = Plan(items=[WorkItemSpec(agent_name="scout", local_id="s1")], rationale="r")
    stub, _ = _make_stub_agent(submitted=plan)
    _patch_spawn(monkeypatch, stub)

    res = await run_subagent.execute(
        run_subagent.input_model(agent_name="subagent", prompt="plan it"), _ctx()
    )
    assert not res.is_error
    env = json.loads(res.output)
    assert env["kind"] == "plan"
    assert env["payload"]["rationale"] == "r"
    assert env["payload"]["items"][0]["agent_name"] == "scout"


@pytest.mark.asyncio
async def test_envelope_kind_raw_when_no_posthook_submission(monkeypatch):
    stub, _ = _make_stub_agent(submitted=None, final_text="hello world")
    _patch_spawn(monkeypatch, stub)

    res = await run_subagent.execute(run_subagent.input_model(agent_name="subagent", prompt="x"), _ctx())
    assert not res.is_error
    env = json.loads(res.output)
    assert env["kind"] == "raw"
    assert env["payload"]["final_text"] == "hello world"


@pytest.mark.asyncio
async def test_configured_posthook_runs_when_work_phase_only_returns_raw_text(monkeypatch):
    work_stub, _ = _make_stub_agent(
        submitted=None,
        final_text='{"summary":"scout report","artifact":{"target_paths":["src/auth"],"files":[]}}',
    )
    serializer_stub, captured = _make_stub_agent(
        submitted=SubmittedSummary(
            summary="scout report",
            artifact={"target_paths": ["src/auth"], "files": []},
        ),
        final_text="submitted",
    )

    def _fake_spawn(*args, **kwargs):
        agent_name = kwargs["agent_def"].name
        if agent_name == "submit_summary_agent":
            return serializer_stub
        return work_stub

    monkeypatch.setattr("engine.runtime.agent.spawn_agent", _fake_spawn, raising=True)

    res = await run_subagent.execute(
        run_subagent.input_model(agent_name="scout", input={"target_paths": ["src/auth"]}),
        _ctx(),
    )

    assert not res.is_error
    env = json.loads(res.output)
    assert env["kind"] == "brief"
    assert env["summary"] == "scout report"
    assert env["payload"]["target_paths"] == ["src/auth"]
    assert captured["prompt"].startswith("{")


@pytest.mark.asyncio
async def test_direct_submission_uses_configured_metadata_key(monkeypatch):
    register_definition(
        AgentDefinition(
            name="custom_submitter",
            description="d",
            agent_type="subagent",
            posthook=PosthookConfig(
                agent_name="submit_summary_agent",
                metadata_key="submitted_custom",
            ),
        )
    )

    class _DirectSubmitter:
        def __init__(self) -> None:
            self.display_messages: list[Any] = []
            self.query_context = SimpleNamespace(
                tool_metadata=ExecutionMetadata(),
                api_messages_snapshot=None,
            )

        async def run(self, prompt: str):  # type: ignore[no-untyped-def]
            from message.messages import ConversationMessage, TextBlock

            key = self.query_context.tool_metadata["posthook_metadata_key"]
            self.query_context.tool_metadata[key] = {"custom": ["src/auth/service.py"]}
            self.display_messages.append(
                ConversationMessage(role="assistant", content=[TextBlock(text="custom output")])
            )
            if False:  # pragma: no cover - generator stub
                yield None
            return

    try:
        monkeypatch.setattr(
            "engine.runtime.agent.spawn_agent",
            lambda *a, **k: _DirectSubmitter(),
            raising=True,
        )

        res = await run_subagent.execute(
            run_subagent.input_model(agent_name="custom_submitter", prompt="x"),
            _ctx(),
        )

        assert not res.is_error
        env = json.loads(res.output)
        assert env["kind"] == "summary"
        assert env["summary"] == "custom output"
        assert env["payload"] == {"custom": ["src/auth/service.py"]}
    finally:
        unregister_definition("custom_submitter")


@pytest.mark.asyncio
async def test_misconfigured_serializer_is_rejected_before_work_phase(monkeypatch):
    register_definition(
        AgentDefinition(
            name="bad_submitter",
            description="bad",
            toolkits=["submit_summary_posthook"],
            include_skills=True,
            skills=[],
            agent_type="subagent",
        )
    )
    register_definition(
        AgentDefinition(
            name="worker_with_bad_posthook",
            description="d",
            agent_type="subagent",
            posthook=PosthookConfig(
                agent_name="bad_submitter",
                metadata_key="submitted_custom",
            ),
        )
    )
    try:
        def _should_not_spawn(*args, **kwargs):
            raise AssertionError("spawn_agent should not be called for invalid serializer config")

        monkeypatch.setattr(
            "engine.runtime.agent.spawn_agent",
            _should_not_spawn,
            raising=True,
        )

        res = await run_subagent.execute(
            run_subagent.input_model(agent_name="worker_with_bad_posthook", prompt="x"),
            _ctx(),
        )

        assert res.is_error
        assert "must not be equipped with builtin skills" in res.output
    finally:
        unregister_definition("worker_with_bad_posthook")
        unregister_definition("bad_submitter")


# ---------- shared_briefings inheritance --------------------------------------


@pytest.mark.asyncio
async def test_shared_briefings_prepended_to_subagent_prompt(monkeypatch):
    budgets = BudgetConfig()
    state = BudgetState()
    artifacts = InMemoryArtifactStore(budgets, state)
    artifacts.save("A1", {"target_paths": ["src/auth"], "summary": "auth map"})
    pc = ProjectContext(goal="g", user_request="u")
    pc.shared_briefings = {
        "src/auth": Briefing(name="auth_map", source="artifact", ref="A1")
    }
    team_run = SimpleNamespace(
        id="T-shared",
        budgets=budgets,
        artifacts=artifacts,
        project_context=pc,
    )
    _register_team_run(team_run)

    stub, captured = _make_stub_agent(
        submitted=SubmittedSummary(summary="ok", artifact=None)
    )
    _patch_spawn(monkeypatch, stub)

    try:
        res = await run_subagent.execute(
            run_subagent.input_model(agent_name="subagent", prompt="explore the area"),
            _ctx(team_run_id="T-shared"),
        )
        assert not res.is_error
        # The subagent saw the shared briefing preamble before its task body.
        assert "auth map" in captured["prompt"]
        assert captured["prompt"].endswith("explore the area")
    finally:
        _unregister_team_run("T-shared")


# ---------- scout builtin registration ----------------------------------------


def test_scout_builtin_registered():
    from agents import get_definition

    scout = get_definition("scout")
    assert scout is not None
    assert scout.agent_type == "subagent"
    assert scout.tool_call_limit == 100
    assert "code_intelligence" in scout.toolkits
    assert scout.posthook is not None
