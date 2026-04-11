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
from team.context.scout_briefings import store_stable_scout_artifact

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
from tools.subagent import RestrictedRunSubagentTool
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


def _seed_context_pressure(team_run: SimpleNamespace, scope: str) -> None:
    team_run.project_context.scope_context_stats[scope] = {
        "lane_ids": {"developer-lane", "validator-lane"},
        "roles": {"developer", "validator"},
        "source_refs": {"payload:owned_files", "dep:auth-map"},
        "read_paths": {f"{scope}/service.py"},
        "verify_refs": {"tests/test_auth.py"},
        "failure_refs": set(),
        "developer_lane_ids": {"developer-lane"},
        "validator_after_developer": True,
    }


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
async def test_scout_rejects_overlapping_prior_scout_coverage(monkeypatch):
    ctx = _ctx()
    ctx.metadata["_scout_target_paths_this_turn"] = ["/testbed/pydantic"]

    res = await run_subagent.execute(
        run_subagent.input_model(
            agent_name="scout",
            input={"target_paths": ["/testbed/pydantic/json_schema.py"]},
        ),
        ctx,
    )

    assert res.is_error
    assert "overlap a scope already covered in this turn" in res.output
    assert "/testbed/pydantic" in res.output


@pytest.mark.asyncio
async def test_scout_ignores_its_own_launch_trace_when_running_in_background(monkeypatch):
    submitted = SubmittedSummary(
        summary="scout report",
        artifact={"target_paths": ["/testbed/pydantic/json_schema.py"], "files": []},
    )
    stub, _ = _make_stub_agent(submitted=submitted)
    _patch_spawn(monkeypatch, stub)

    ctx = _ctx()
    ctx.metadata.tool_id = "toolu_self"
    ctx.metadata["_scout_target_paths_this_turn"] = ["/testbed/pydantic/json_schema.py"]
    ctx.metadata["_scout_trace_targets_by_tool_use_id"] = {
        "toolu_self": ["/testbed/pydantic/json_schema.py"]
    }

    res = await run_subagent.execute(
        run_subagent.input_model(
            agent_name="scout",
            input={"target_paths": ["/testbed/pydantic/json_schema.py"]},
        ),
        ctx,
    )

    assert not res.is_error


@pytest.mark.asyncio
async def test_scout_allows_parallel_fanout_when_live_scope_prefers_serialization(monkeypatch):
    submitted = SubmittedSummary(
        summary="scout report",
        artifact={"target_paths": ["/testbed/pydantic/json_schema.py"], "files": []},
    )
    stub, _ = _make_stub_agent(submitted=submitted)
    _patch_spawn(monkeypatch, stub)

    ctx = _ctx()
    ctx.metadata["coordination_mode"] = "ultra"
    ctx.metadata["_scout_target_paths_this_turn"] = ["/testbed/pydantic/core.py"]
    monkeypatch.setattr(
        "tools.subagent.run_subagent_tool.build_scope_packet_for_context",
        lambda *a, **k: {
            "scope_paths": ["/testbed/pydantic/json_schema.py"],
            "coherence_token": "token-1",
            "admission": {
                "mode": "serialize",
                "allow_parallel_fanout": False,
                "reasons": ["active write reservations overlap this scope"],
            },
        },
    )

    res = await run_subagent.execute(
        run_subagent.input_model(
            agent_name="scout",
            input={"target_paths": ["/testbed/pydantic/json_schema.py"]},
        ),
        ctx,
    )

    assert not res.is_error


@pytest.mark.asyncio
async def test_scout_allows_turn_fanout_beyond_old_cap(monkeypatch):
    submitted = SubmittedSummary(
        summary="scout report",
        artifact={"target_paths": ["/testbed/pydantic/json_schema.py"], "files": []},
    )
    stub, _ = _make_stub_agent(submitted=submitted)
    _patch_spawn(monkeypatch, stub)

    ctx = _ctx()
    ctx.metadata["coordination_mode"] = "ultra"
    ctx.metadata["_scout_launches_this_turn"] = 8

    res = await run_subagent.execute(
        run_subagent.input_model(
            agent_name="scout",
            input={"target_paths": ["/testbed/pydantic/json_schema.py"]},
        ),
        ctx,
    )

    assert not res.is_error


@pytest.mark.asyncio
async def test_scout_allows_benchmark_root_launch_before_scope_status_when_prompt_allows(monkeypatch):
    submitted = SubmittedSummary(
        summary="scout report",
        artifact={"target_paths": ["pkg/core.py"], "files": []},
    )
    stub, _ = _make_stub_agent(submitted=submitted)
    _patch_spawn(monkeypatch, stub)

    ctx = _ctx(team_run_id="TR_BENCH")
    ctx.metadata["agent_name"] = "team_planner"
    ctx.metadata["work_item_id"] = "ROOT"
    team_run = SimpleNamespace(
        root_work_item_id="ROOT",
        dispatcher=SimpleNamespace(
            graph={
                "ROOT": SimpleNamespace(
                    payload={"fail_to_pass": ["pkg/tests/test_api.py::test_one"]}
                )
            }
        ),
    )
    monkeypatch.setattr("team.runtime.registry.get", lambda team_run_id: team_run if team_run_id == "TR_BENCH" else None)

    res = await run_subagent.execute(
        run_subagent.input_model(
            agent_name="scout",
            input={"target_paths": ["pkg/core.py"]},
        ),
        ctx,
    )

    assert not res.is_error


@pytest.mark.asyncio
async def test_restricted_run_subagent_tool_forwards_background_preflight(monkeypatch):
    ctx = _ctx(team_run_id="TR_BENCH")
    ctx.metadata["agent_name"] = "team_planner"
    ctx.metadata["work_item_id"] = "ROOT"
    team_run = SimpleNamespace(
        root_work_item_id="ROOT",
        dispatcher=SimpleNamespace(
            graph={
                "ROOT": SimpleNamespace(
                    payload={"fail_to_pass": ["pkg/tests/test_api.py::test_one"]}
                )
            }
        ),
    )
    monkeypatch.setattr(
        "team.runtime.registry.get",
        lambda team_run_id: team_run if team_run_id == "TR_BENCH" else None,
    )
    tool = RestrictedRunSubagentTool(allowed_agent_names=("scout",))

    res = tool.background_preflight(
        tool.input_model(agent_name="scout", input={"target_paths": ["pkg/core.py"]}),
        ctx,
    )

    assert res is None


@pytest.mark.asyncio
async def test_scout_rejects_benchmark_test_file_after_root_scope_anchor(monkeypatch):
    ctx = _ctx(team_run_id="TR_BENCH")
    ctx.metadata["agent_name"] = "team_planner"
    ctx.metadata["work_item_id"] = "ROOT"
    ctx.metadata["_benchmark_root_scope_anchor_done"] = True
    team_run = SimpleNamespace(
        root_work_item_id="ROOT",
        dispatcher=SimpleNamespace(
            graph={
                "ROOT": SimpleNamespace(
                    payload={"fail_to_pass": ["pkg/tests/test_api.py::test_one"]}
                )
            }
        ),
    )
    monkeypatch.setattr("team.runtime.registry.get", lambda team_run_id: team_run if team_run_id == "TR_BENCH" else None)

    res = await run_subagent.execute(
        run_subagent.input_model(
            agent_name="scout",
            input={"target_paths": ["pkg/tests/test_api.py"]},
        ),
        ctx,
    )

    assert res.is_error
    assert "benchmark test scopes are failure evidence" in res.output


@pytest.mark.asyncio
async def test_scout_rejects_benchmark_test_directory_after_root_scope_anchor(monkeypatch):
    ctx = _ctx(team_run_id="TR_BENCH")
    ctx.metadata["agent_name"] = "team_planner"
    ctx.metadata["work_item_id"] = "ROOT"
    ctx.metadata["_benchmark_root_scope_anchor_done"] = True
    team_run = SimpleNamespace(
        root_work_item_id="ROOT",
        dispatcher=SimpleNamespace(
            graph={
                "ROOT": SimpleNamespace(
                    payload={"fail_to_pass": ["pkg/tests/test_api.py::test_one"]}
                )
            }
        ),
    )
    monkeypatch.setattr(
        "team.runtime.registry.get",
        lambda team_run_id: team_run if team_run_id == "TR_BENCH" else None,
    )

    res = await run_subagent.execute(
        run_subagent.input_model(
            agent_name="scout",
            input={"target_paths": ["pkg/tests"]},
        ),
        ctx,
    )

    assert res.is_error
    assert "benchmark test scopes are failure evidence" in res.output


@pytest.mark.asyncio
async def test_scout_allows_missing_benchmark_root_owner_path(monkeypatch):
    submitted = SubmittedSummary(
        summary="scout report",
        artifact={"target_paths": ["pkg/io/parquet.py"], "files": []},
    )
    stub, _ = _make_stub_agent(submitted=submitted)
    _patch_spawn(monkeypatch, stub)

    ctx = _ctx(team_run_id="TR_BENCH")
    ctx.metadata["agent_name"] = "team_planner"
    ctx.metadata["work_item_id"] = "ROOT"
    ctx.metadata["_benchmark_root_scope_anchor_done"] = True
    team_run = SimpleNamespace(
        root_work_item_id="ROOT",
        dispatcher=SimpleNamespace(
            graph={
                "ROOT": SimpleNamespace(
                    payload={"fail_to_pass": ["pkg/tests/test_api.py::test_one"]}
                )
            }
        ),
    )
    monkeypatch.setattr(
        "team.runtime.registry.get",
        lambda team_run_id: team_run if team_run_id == "TR_BENCH" else None,
    )
    res = await run_subagent.execute(
        run_subagent.input_model(
            agent_name="scout",
            input={"target_paths": ["pkg/io/parquet.py"]},
        ),
        ctx,
    )

    assert not res.is_error


@pytest.mark.asyncio
async def test_scout_allows_third_root_first_wave_lane_before_any_completion(monkeypatch):
    submitted = SubmittedSummary(
        summary="scout report",
        artifact={"target_paths": ["pkg/third_owner.py"], "files": []},
    )
    stub, _ = _make_stub_agent(submitted=submitted)
    _patch_spawn(monkeypatch, stub)

    ctx = _ctx(team_run_id="TR_BENCH")
    ctx.metadata["agent_name"] = "team_planner"
    ctx.metadata["work_item_id"] = "ROOT"
    ctx.metadata["_benchmark_root_scope_anchor_done"] = True
    ctx.metadata["_scout_launches_this_turn"] = 2
    team_run = SimpleNamespace(
        root_work_item_id="ROOT",
        dispatcher=SimpleNamespace(
            graph={
                "ROOT": SimpleNamespace(
                    payload={"fail_to_pass": ["pkg/tests/test_api.py::test_one"]}
                )
            }
        ),
    )
    monkeypatch.setattr("team.runtime.registry.get", lambda team_run_id: team_run if team_run_id == "TR_BENCH" else None)

    res = await run_subagent.execute(
        run_subagent.input_model(
            agent_name="scout",
            input={"target_paths": ["pkg/third_owner.py"]},
        ),
        ctx,
    )

    assert not res.is_error


@pytest.mark.asyncio
async def test_scout_allows_fourth_root_first_wave_lane_before_any_completion(monkeypatch):
    submitted = SubmittedSummary(
        summary="scout report",
        artifact={"target_paths": ["pkg/fourth_owner.py"], "files": []},
    )
    stub, _ = _make_stub_agent(submitted=submitted)
    _patch_spawn(monkeypatch, stub)

    ctx = _ctx(team_run_id="TR_BENCH")
    ctx.metadata["agent_name"] = "team_planner"
    ctx.metadata["work_item_id"] = "ROOT"
    ctx.metadata["_benchmark_root_scope_anchor_done"] = True
    ctx.metadata["_scout_launches_this_turn"] = 3
    team_run = SimpleNamespace(
        root_work_item_id="ROOT",
        dispatcher=SimpleNamespace(
            graph={
                "ROOT": SimpleNamespace(
                    payload={"fail_to_pass": ["pkg/tests/test_api.py::test_one"]}
                )
            }
        ),
    )
    monkeypatch.setattr("team.runtime.registry.get", lambda team_run_id: team_run if team_run_id == "TR_BENCH" else None)

    res = await run_subagent.execute(
        run_subagent.input_model(
            agent_name="scout",
            input={"target_paths": ["pkg/fourth_owner.py"]},
        ),
        ctx,
    )

    assert not res.is_error


@pytest.mark.asyncio
async def test_scout_allows_fifth_root_first_wave_lane_before_any_completion(monkeypatch):
    submitted = SubmittedSummary(
        summary="scout report",
        artifact={"target_paths": ["pkg/fifth_owner.py"], "files": []},
    )
    stub, _ = _make_stub_agent(submitted=submitted)
    _patch_spawn(monkeypatch, stub)

    ctx = _ctx(team_run_id="TR_BENCH")
    ctx.metadata["agent_name"] = "team_planner"
    ctx.metadata["work_item_id"] = "ROOT"
    ctx.metadata["_benchmark_root_scope_anchor_done"] = True
    ctx.metadata["_scout_launches_this_turn"] = 5
    team_run = SimpleNamespace(
        root_work_item_id="ROOT",
        dispatcher=SimpleNamespace(
            graph={
                "ROOT": SimpleNamespace(
                    payload={"fail_to_pass": ["pkg/tests/test_api.py::test_one"]}
                )
            }
        ),
    )
    monkeypatch.setattr("team.runtime.registry.get", lambda team_run_id: team_run if team_run_id == "TR_BENCH" else None)

    res = await run_subagent.execute(
        run_subagent.input_model(
            agent_name="scout",
            input={"target_paths": ["pkg/fifth_owner.py"]},
        ),
        ctx,
    )

    assert not res.is_error


@pytest.mark.asyncio
async def test_scout_allows_admitted_first_wave_lane_when_launch_order_is_recorded(monkeypatch):
    submitted = SubmittedSummary(
        summary="scout report",
        artifact={"target_paths": ["pkg/fourth_owner.py"], "files": []},
    )
    stub, _ = _make_stub_agent(submitted=submitted)
    _patch_spawn(monkeypatch, stub)

    ctx = _ctx(team_run_id="TR_BENCH")
    ctx.metadata["agent_name"] = "team_planner"
    ctx.metadata["work_item_id"] = "ROOT"
    ctx.metadata["_benchmark_root_scope_anchor_done"] = True
    ctx.metadata["_scout_launches_this_turn"] = 4
    ctx.metadata.tool_id = "toolu_fourth"
    ctx.metadata["_scout_trace_targets_by_tool_use_id"] = {
        "toolu_first": ["pkg/first_owner.py"],
        "toolu_second": ["pkg/second_owner.py"],
        "toolu_third": ["pkg/third_owner.py"],
        "toolu_fourth": ["pkg/fourth_owner.py"],
    }
    ctx.metadata["_scout_launch_order_by_tool_use_id"] = {
        "toolu_first": 1,
        "toolu_second": 2,
        "toolu_third": 3,
        "toolu_fourth": 4,
    }
    team_run = SimpleNamespace(
        root_work_item_id="ROOT",
        dispatcher=SimpleNamespace(
            graph={
                "ROOT": SimpleNamespace(
                    payload={"fail_to_pass": ["pkg/tests/test_api.py::test_one"]}
                )
            }
        ),
    )
    monkeypatch.setattr(
        "team.runtime.registry.get",
        lambda team_run_id: team_run if team_run_id == "TR_BENCH" else None,
    )

    res = await run_subagent.execute(
        run_subagent.input_model(
            agent_name="scout",
            input={"target_paths": ["pkg/fourth_owner.py"]},
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
    assert env["run_id"]
    assert env["artifact_ref"] is None
    assert env["summary"] == "scout report"
    assert env["payload"]["target_paths"] == ["src/auth"]


@pytest.mark.asyncio
async def test_scout_injects_scope_packet_into_prompt_and_metadata(monkeypatch):
    submitted = SubmittedSummary(
        summary="scout report",
        artifact={"target_paths": ["src/auth"], "files": []},
    )
    stub, captured = _make_stub_agent(submitted=submitted)
    _patch_spawn(monkeypatch, stub)
    monkeypatch.setattr(
        "tools.subagent.run_subagent_tool.build_scope_packet_for_context",
        lambda *a, **k: {
            "scope_paths": ["src/auth"],
            "coherence_token": "token-1",
            "admission": {"mode": "parallel", "allow_parallel_fanout": True},
        },
    )
    monkeypatch.setattr(
        "tools.subagent.run_subagent_tool.render_scope_packet",
        lambda packet: f"SCOPE {packet['coherence_token']}",
    )

    res = await run_subagent.execute(
        run_subagent.input_model(agent_name="scout", input={"target_paths": ["src/auth"]}),
        _ctx(),
    )

    assert not res.is_error
    assert "SCOPE token-1\n\n" in captured["prompt"]
    assert stub.query_context.tool_metadata["coherence_token"] == "token-1"
    assert stub.query_context.tool_metadata["scope_packet"]["scope_paths"] == ["src/auth"]


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
    assert env["run_id"]
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
    assert env["run_id"]
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
    assert env["run_id"]
    assert env["payload"]["final_text"] == "hello world"


@pytest.mark.asyncio
async def test_team_scout_returns_stable_artifact_ref_and_auto_promotes(monkeypatch):
    budgets = BudgetConfig()
    state = BudgetState()
    artifacts = InMemoryArtifactStore(budgets, state)
    team_run = SimpleNamespace(
        id="T-scout",
        budgets=budgets,
        artifacts=artifacts,
        project_context=ProjectContext(goal="g", user_request="u"),
    )
    _seed_context_pressure(team_run, "src/auth")
    _register_team_run(team_run)

    submitted = SubmittedSummary(
        summary="scout report",
        artifact={
            "target_paths": ["src/auth"],
            "canonical_scope": "src/auth",
            "files": [],
            "scope_coverage": 1.0,
            "gaps": "",
            "suggested_subdivisions": [],
            "snapshot_time": 100.0,
        },
    )
    stub, _ = _make_stub_agent(submitted=submitted)
    _patch_spawn(monkeypatch, stub)

    try:
        res = await run_subagent.execute(
            run_subagent.input_model(agent_name="scout", input={"target_paths": ["src/auth"]}),
            _ctx(team_run_id="T-scout"),
        )
        assert not res.is_error
        env = json.loads(res.output)
        assert env["run_id"]
        assert env["artifact_ref"] == "scout:src/auth"
        assert artifacts.load("scout:src/auth")["canonical_scope"] == "src/auth"
        shared = team_run.project_context.shared_briefings["src/auth"]
        assert shared.ref == "scout:src/auth"
        assert "context_hotspot_score" in (shared.description or "")
    finally:
        _unregister_team_run("T-scout")


@pytest.mark.asyncio
async def test_team_scout_normalizes_missing_empty_contract_fields(monkeypatch):
    budgets = BudgetConfig()
    state = BudgetState()
    artifacts = InMemoryArtifactStore(budgets, state)
    team_run = SimpleNamespace(
        id="T-scout-normalized",
        budgets=budgets,
        artifacts=artifacts,
        project_context=ProjectContext(goal="g", user_request="u"),
    )
    _register_team_run(team_run)

    submitted = SubmittedSummary(
        summary="scout report",
        artifact={
            "target_paths": ["src/auth"],
            "canonical_scope": "src/auth",
            "files": [],
            "scope_coverage": 1.0,
        },
    )
    stub, _ = _make_stub_agent(submitted=submitted)
    _patch_spawn(monkeypatch, stub)

    try:
        res = await run_subagent.execute(
            run_subagent.input_model(agent_name="scout", input={"target_paths": ["src/auth"]}),
            _ctx(team_run_id="T-scout-normalized"),
        )
        assert not res.is_error
        env = json.loads(res.output)
        assert env["artifact_ref"] == "scout:src/auth"
        stored = artifacts.load("scout:src/auth")
        assert stored["entry_points"] == []
        assert stored["open_questions"] == []
        assert stored["gaps"] == ""
        assert stored["suggested_subdivisions"] == []
    finally:
        _unregister_team_run("T-scout-normalized")


@pytest.mark.asyncio
async def test_team_scout_does_not_overwrite_newer_stable_artifact(monkeypatch):
    budgets = BudgetConfig()
    state = BudgetState()
    artifacts = InMemoryArtifactStore(budgets, state)
    artifacts.save(
        "scout:src/auth",
        {
            "target_paths": ["src/auth"],
            "canonical_scope": "src/auth",
            "summary": "newer brief",
            "files": [],
            "scope_coverage": 1.0,
            "gaps": "",
            "suggested_subdivisions": [],
            "snapshot_time": 200.0,
        },
    )
    team_run = SimpleNamespace(
        id="T-scout-guard",
        budgets=budgets,
        artifacts=artifacts,
        project_context=ProjectContext(goal="g", user_request="u"),
    )
    _register_team_run(team_run)

    submitted = SubmittedSummary(
        summary="older scout report",
        artifact={
            "target_paths": ["src/auth"],
            "canonical_scope": "src/auth",
            "summary": "older brief",
            "files": [],
            "scope_coverage": 1.0,
            "gaps": "",
            "suggested_subdivisions": [],
            "snapshot_time": 100.0,
        },
    )
    stub, _ = _make_stub_agent(submitted=submitted)
    _patch_spawn(monkeypatch, stub)

    try:
        res = await run_subagent.execute(
            run_subagent.input_model(agent_name="scout", input={"target_paths": ["src/auth"]}),
            _ctx(team_run_id="T-scout-guard"),
        )
        assert not res.is_error
        env = json.loads(res.output)
        assert env["artifact_ref"] == "scout:src/auth"
        assert artifacts.load("scout:src/auth")["summary"] == "newer brief"
    finally:
        _unregister_team_run("T-scout-guard")


def test_stable_scout_replacement_uses_run_id_tie_break_for_equal_snapshots():
    budgets = BudgetConfig()
    state = BudgetState()
    artifacts = InMemoryArtifactStore(budgets, state)
    team_run = SimpleNamespace(
        id="T-scout-order",
        budgets=budgets,
        artifacts=artifacts,
        project_context=ProjectContext(goal="g", user_request="u"),
    )

    ref = store_stable_scout_artifact(
        team_run,
        {
            "target_paths": ["src/auth"],
            "canonical_scope": "src/auth",
            "summary": "run-b",
            "files": [],
            "scope_coverage": 1.0,
            "gaps": "",
            "suggested_subdivisions": [],
            "snapshot_time": 100.0,
        },
        run_id="run-b",
    )

    assert ref == "scout:src/auth"
    store_stable_scout_artifact(
        team_run,
        {
            "target_paths": ["src/auth"],
            "canonical_scope": "src/auth",
            "summary": "run-a",
            "files": [],
            "scope_coverage": 1.0,
            "gaps": "",
            "suggested_subdivisions": [],
            "snapshot_time": 100.0,
        },
        run_id="run-a",
    )
    assert artifacts.load("scout:src/auth")["summary"] == "run-b"

    store_stable_scout_artifact(
        team_run,
        {
            "target_paths": ["src/auth"],
            "canonical_scope": "src/auth",
            "summary": "run-z",
            "files": [],
            "scope_coverage": 1.0,
            "gaps": "",
            "suggested_subdivisions": [],
            "snapshot_time": 100.0,
        },
        run_id="run-z",
    )
    assert artifacts.load("scout:src/auth")["summary"] == "run-z"


def test_stable_scout_replacement_keeps_current_when_tie_provenance_is_missing():
    budgets = BudgetConfig()
    state = BudgetState()
    artifacts = InMemoryArtifactStore(budgets, state)
    artifacts.save(
        "scout:src/auth",
        {
            "target_paths": ["src/auth"],
            "canonical_scope": "src/auth",
            "summary": "existing",
            "files": [],
            "scope_coverage": 1.0,
            "gaps": "",
            "suggested_subdivisions": [],
        },
    )
    team_run = SimpleNamespace(
        id="T-scout-missing-order",
        budgets=budgets,
        artifacts=artifacts,
        project_context=ProjectContext(goal="g", user_request="u"),
    )

    ref = store_stable_scout_artifact(
        team_run,
        {
            "target_paths": ["src/auth"],
            "canonical_scope": "src/auth",
            "summary": "incoming",
            "files": [],
            "scope_coverage": 1.0,
            "gaps": "",
            "suggested_subdivisions": [],
        },
        run_id="run-z",
    )

    assert ref == "scout:src/auth"
    assert artifacts.load("scout:src/auth")["summary"] == "existing"


def test_stable_scout_replacement_uses_run_id_tie_break_when_snapshots_are_missing():
    budgets = BudgetConfig()
    state = BudgetState()
    artifacts = InMemoryArtifactStore(budgets, state)
    team_run = SimpleNamespace(
        id="T-scout-missing-snapshot-order",
        budgets=budgets,
        artifacts=artifacts,
        project_context=ProjectContext(goal="g", user_request="u"),
    )

    store_stable_scout_artifact(
        team_run,
        {
            "target_paths": ["src/auth"],
            "canonical_scope": "src/auth",
            "summary": "run-b",
            "files": [],
            "scope_coverage": 1.0,
            "gaps": "",
            "suggested_subdivisions": [],
        },
        run_id="run-b",
    )
    store_stable_scout_artifact(
        team_run,
        {
            "target_paths": ["src/auth"],
            "canonical_scope": "src/auth",
            "summary": "run-a",
            "files": [],
            "scope_coverage": 1.0,
            "gaps": "",
            "suggested_subdivisions": [],
        },
        run_id="run-a",
    )
    assert artifacts.load("scout:src/auth")["summary"] == "run-b"

    store_stable_scout_artifact(
        team_run,
        {
            "target_paths": ["src/auth"],
            "canonical_scope": "src/auth",
            "summary": "run-z",
            "files": [],
            "scope_coverage": 1.0,
            "gaps": "",
            "suggested_subdivisions": [],
        },
        run_id="run-z",
    )
    assert artifacts.load("scout:src/auth")["summary"] == "run-z"


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
async def test_scout_stable_version_ignores_ephemeral_fallback_run_id(monkeypatch):
    budgets = BudgetConfig()
    state = BudgetState()
    artifacts = InMemoryArtifactStore(budgets, state)
    team_run = SimpleNamespace(
        id="T-scout-ephemeral",
        budgets=budgets,
        artifacts=artifacts,
        project_context=ProjectContext(goal="g", user_request="u"),
        note_direct_scout_brief=lambda *args, **kwargs: None,
    )
    _register_team_run(team_run)
    submitted = SubmittedSummary(
        summary="scout report",
        artifact={
            "target_paths": ["src/auth"],
            "canonical_scope": "src/auth",
            "summary": "fresh scout",
            "files": [],
            "scope_coverage": 1.0,
            "gaps": "",
            "suggested_subdivisions": [],
            "snapshot_time": 100.0,
        },
    )
    stub, _ = _make_stub_agent(submitted=submitted)
    _patch_spawn(monkeypatch, stub)

    class _UnavailableRunStore:
        is_ready = False

    monkeypatch.setattr("server.app_factory.agent_run_store", _UnavailableRunStore(), raising=True)

    try:
        res = await run_subagent.execute(
            run_subagent.input_model(agent_name="scout", input={"target_paths": ["src/auth"]}),
            _ctx(team_run_id="T-scout-ephemeral"),
        )
        assert not res.is_error
        env = json.loads(res.output)
        assert env["run_id"].startswith("ephemeral-")
        assert env["artifact_ref"] == "scout:src/auth"
        assert team_run.project_context.stable_scout_versions["src/auth"] == {
            "snapshot_time": 100.0,
        }
    finally:
        _unregister_team_run("T-scout-ephemeral")


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
