"""Tests for the production ``build_query_context`` wiring (Step 2c)."""

from __future__ import annotations

from dataclasses import dataclass
from types import SimpleNamespace

import team.runtime.context_builder as context_builder_module
from team.artifacts.store import InMemoryArtifactStore
from team.context.project import ProjectContext
from team.models import (
    Briefing,
    BudgetConfig,
    BudgetState,
    DependencyArtifact,
    WorkItem,
    WorkItemStatus,
)
from team.runtime.context_builder import (
    TeamAgentContext,
    build_initial_user_message,
    build_query_context,
    default_base_prompt,
    render_work_item_payload,
)


@dataclass
class _FakeDispatcher:
    artifact_store: InMemoryArtifactStore


def _fake_team_run(
    artifact_store: InMemoryArtifactStore,
    *,
    sandbox_id: str = "",
    repo_root: str = "",
) -> SimpleNamespace:
    project_context = ProjectContext(goal="g", user_request="u", repo_root=repo_root)
    return SimpleNamespace(
        id="T1",
        sandbox_id=sandbox_id,
        dispatcher=_FakeDispatcher(artifact_store=artifact_store),
        project_context=project_context,
        budgets=BudgetConfig(),
    )


def _wi(**over) -> WorkItem:
    base = dict(id="W1", team_run_id="T1", agent_name="worker", status=WorkItemStatus.READY)
    base.update(over)
    return WorkItem(**base)


def test_default_base_prompt_uses_task_key():
    out = default_base_prompt(_wi(payload={"task": "do it"}))
    assert "do it" in out
    assert '"task": "do it"' in out


def test_render_work_item_payload_keeps_structured_fields():
    out = render_work_item_payload(
        {
            "description": "refresh atlas",
            "stale_subsystems": ["pydantic/core_models"],
            "file_path": "/testbed/pydantic/json_schema.py",
        }
    )
    assert out is not None
    assert "refresh atlas" in out
    assert "stale_subsystems" in out
    assert "pydantic/core_models" in out
    assert "/testbed/pydantic/json_schema.py" in out


def test_default_base_prompt_fallback():
    out = default_base_prompt(_wi(payload={}))
    assert "W1" in out and "worker" in out


def test_default_base_prompt_uses_payload_for_normal_work_without_replan_source():
    out = default_base_prompt(_wi(payload={"legacy_flag": True, "task": "do it"}))
    assert "do it" in out
    assert "Failed work item" not in out


def test_default_base_prompt_uses_replan_source_id_for_replanner():
    out = default_base_prompt(
        _wi(
            agent_name="team_replanner",
            replan_source_id="VAL1",
            payload={
                "failed_work_item_id": "VAL1",
                "failed_agent": "validator",
                "failure_reason": "tests failed",
                "failure_context": "traceback",
                "suggestion": "add a fix task",
                "original_payload": {"verify": ["pytest -q"]},
            },
        )
    )
    assert "Replan Request" in out
    assert "VAL1" in out
    assert "traceback" in out


def test_build_initial_user_message_no_briefings():
    store = InMemoryArtifactStore(BudgetConfig(), BudgetState())
    tr = _fake_team_run(store)
    msg = build_initial_user_message(tr, _wi(), "base")
    assert msg == "base"


def test_build_initial_user_message_prepends_briefings():
    store = InMemoryArtifactStore(BudgetConfig(), BudgetState())
    store.save("A1", "brief body")
    tr = _fake_team_run(store)
    wi = _wi(briefings=[Briefing(name="ctx", source="artifact", ref="A1")])
    msg = build_initial_user_message(tr, wi, "task text")
    assert "brief body" in msg
    assert msg.endswith("task text")


def test_build_query_context_carries_team_metadata_and_briefings():
    store = InMemoryArtifactStore(BudgetConfig(), BudgetState())
    store.save("P", {"target_paths": ["src"], "summary": "scout report"})
    tr = _fake_team_run(store)
    wi = _wi(
        payload={"task": "implement", "target_paths": ["src/file.py"]},
        dep_artifacts=[
            DependencyArtifact(source_wi_id="P", artifact_ref="P", display_name="scout_1")
        ],
    )
    defn = SimpleNamespace(name="worker")
    ctx = build_query_context(defn, tr, wi)
    assert isinstance(ctx, TeamAgentContext)
    assert "scout report" in ctx.user_message
    assert "implement" in ctx.user_message
    assert '"task": "implement"' in ctx.user_message
    assert ctx.tool_metadata.team_run_id == "T1"
    assert ctx.tool_metadata.work_item_id == "W1"
    assert ctx.tool_metadata.agent_run_id is None
    assert isinstance(ctx.tool_metadata.get("work_item_started_at"), float)
    assert ctx.tool_metadata["coordination_mode"] == "ultra"
    assert ctx.tool_metadata["require_declared_shell_outputs"] is True
    assert ctx.tool_metadata["default_scope_paths"] == ["src/file.py"]


def test_shared_briefings_flow_into_query_context():
    store = InMemoryArtifactStore(BudgetConfig(), BudgetState())
    store.save("S1", {"target_paths": ["src/auth"], "summary": "shared scout"})
    tr = _fake_team_run(store)
    tr.project_context.shared_briefings = {
        "src/auth": Briefing(name="auth_map", source="artifact", ref="S1")
    }
    wi = _wi(payload={"task": "refactor auth"})
    defn = SimpleNamespace(name="worker")
    ctx = build_query_context(defn, tr, wi)
    assert "shared scout" in ctx.user_message
    assert "Shared context" in ctx.user_message


def test_build_query_context_injects_scope_packet_when_ci_is_available(monkeypatch):
    store = InMemoryArtifactStore(BudgetConfig(), BudgetState())
    tr = _fake_team_run(store, sandbox_id="sbx-1", repo_root="/testbed")
    wi = _wi(payload={"prompt": "Fix it", "target_paths": ["src/module.py"]})
    defn = SimpleNamespace(name="developer")

    monkeypatch.setattr(context_builder_module, "get_code_intelligence", lambda **_: object())
    monkeypatch.setattr(
        context_builder_module,
        "build_scope_packet",
        lambda **_: {
            "coherence_token": "token-1",
            "freshness": "fresh",
            "scope_paths": ["src/module.py"],
        },
    )
    monkeypatch.setattr(
        context_builder_module,
        "render_scope_packet",
        lambda packet: f"SCOPE {packet['coherence_token']}",
    )

    ctx = build_query_context(defn, tr, wi)

    assert ctx.tool_metadata.sandbox_id == "sbx-1"
    assert ctx.tool_metadata.daytona_cwd == "/testbed"
    assert ctx.tool_metadata["ci_workspace_root"] == "/testbed"
    assert ctx.tool_metadata["default_scope_paths"] == ["src/module.py"]
    assert ctx.tool_metadata["scope_packet"]["coherence_token"] == "token-1"
    assert ctx.tool_metadata["coherence_token"] == "token-1"
    assert ctx.tool_metadata["coordination_mode"] == "ultra"
    assert ctx.tool_metadata["require_declared_shell_outputs"] is True
    assert ctx.user_message.startswith("SCOPE token-1\n\n")


def test_team_agent_context_tracks_posthook_state_outside_raw_metadata():
    ctx = TeamAgentContext(work_result={"phase": "work"})

    ctx.set_posthook_metadata_key("submitted_plan")
    ctx.set_posthook_output("submitted_plan", {"items": []})

    assert ctx.work_result == {"phase": "work"}
    assert ctx.posthook_metadata_key == "submitted_plan"
    assert ctx.get_posthook_output("submitted_plan") == {"items": []}
    assert ctx.tool_metadata["posthook_metadata_key"] == "submitted_plan"
