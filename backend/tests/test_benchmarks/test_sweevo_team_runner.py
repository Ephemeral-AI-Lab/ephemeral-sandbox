from __future__ import annotations

import asyncio
import json
from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest

from benchmarks.sweevo import team_runner as sweevo_team_runner
from benchmarks.sweevo.team_runner import (
    _derive_sweevo_budgets,
    _enforce_validation_evidence,
    _build_agent_overrides,
    _build_root_prompt,
    _derive_planner_runtime_limits,
    _emit_dispatcher_dag,
    _make_context_builders,
    _make_runner,
)
from message import ConversationMessage, TextBlock, ToolUseBlock
from team.builtins import DEVELOPER, TEAM_PLANNER, TEAM_REPLANNER, VALIDATOR
from team.models import WorkItem, WorkItemKind, WorkItemStatus
from tools.core.runtime import ExecutionMetadata


def test_posthook_ctx_prefers_final_text_over_wrapped_work_result():
    _, build_posthook_ctx = _make_context_builders("sbx-1")

    ctx = build_posthook_ctx(
        SimpleNamespace(name="submit_plan_agent"),
        {
            "final_text": '{"items":[{"agent_name":"developer","local_id":"dev1","kind":"atomic"}]}',
            "team_run_id": "T1",
            "work_item_id": "W1",
        },
    )

    assert ctx.user_message == (
        '{"items":[{"agent_name":"developer","local_id":"dev1","kind":"atomic"}]}'
    )
    assert ctx.tool_metadata.team_run_id == "T1"
    assert ctx.tool_metadata.work_item_id == "W1"


def test_posthook_ctx_prefers_extracted_posthook_input_over_final_text():
    _, build_posthook_ctx = _make_context_builders("sbx-1")

    extracted = '{"items":[{"agent_name":"developer","local_id":"dev1","kind":"atomic"}]}'
    ctx = build_posthook_ctx(
        SimpleNamespace(name="submit_plan_agent"),
        {
            "posthook_input_text": extracted,
            "final_text": "Plan payload already submitted. No further action is required.",
            "team_run_id": "T1",
            "work_item_id": "W1",
        },
    )

    assert ctx.user_message == extracted
    assert ctx.tool_metadata.team_run_id == "T1"
    assert ctx.tool_metadata.work_item_id == "W1"


def test_extract_posthook_input_text_recovers_plan_json_with_trailing_prose():
    extracted = sweevo_team_runner._extract_posthook_input_text(
        [
            ConversationMessage(
                role="assistant",
                content=[
                    TextBlock(
                        text=(
                            "I have sufficient evidence.\n\n"
                            '{"items":[{"agent_name":"developer","local_id":"dev1","kind":"atomic"}]}\n\n'
                            "Summary after the payload that should be ignored."
                        )
                    )
                ],
            )
        ],
        "submitted_plan",
    )

    assert extracted is not None
    assert json.loads(extracted) == {
        "items": [{"agent_name": "developer", "local_id": "dev1", "kind": "atomic"}]
    }


def test_extract_posthook_input_text_repairs_malformed_plan_items_missing_outer_braces():
    extracted = sweevo_team_runner._extract_posthook_input_text(
        [
            ConversationMessage(
                role="assistant",
                content=[
                    TextBlock(
                        text=(
                            "I have sufficient evidence.\n\n"
                            '{"items": ['
                            '{"local_id": "dev1", "agent_name": "developer", "kind": "atomic", '
                            '"payload": {"owned_files": ["pydantic/networks.py"]}, '
                            '{"local_id": "planner_residual", "agent_name": "team_planner", '
                            '"kind": "expandable", "payload": {"owned_files": ["pydantic/root_model.py"]}, '
                            '{"local_id": "val1", "agent_name": "validator", "kind": "atomic", '
                            '"deps": ["dev1"], "payload": {"verify": ["tests/test_networks.py"]}}], '
                            '"rationale": "Keep the dominant networks lane isolated."}'
                        )
                    )
                ],
            )
        ],
        "submitted_plan",
    )

    assert extracted is not None
    assert json.loads(extracted) == {
        "items": [
            {
                "local_id": "dev1",
                "agent_name": "developer",
                "kind": "atomic",
                "payload": {"owned_files": ["pydantic/networks.py"]},
            },
            {
                "local_id": "planner_residual",
                "agent_name": "team_planner",
                "kind": "expandable",
                "payload": {"owned_files": ["pydantic/root_model.py"]},
            },
            {
                "local_id": "val1",
                "agent_name": "validator",
                "kind": "atomic",
                "deps": ["dev1"],
                "payload": {"verify": ["tests/test_networks.py"]},
            },
        ],
        "rationale": "Keep the dominant networks lane isolated.",
    }


def test_extract_matching_json_object_prefers_matching_top_level_plan():
    text = (
        '{"items": [{"local_id": "dev1", "agent_name": "developer", "kind": "atomic", '
        '"payload": {"metadata": {"items": ["not-a-plan"]}}}], "rationale": "ok"}'
    )

    payload = sweevo_team_runner._extract_matching_json_object(
        text,
        lambda candidate: sweevo_team_runner._matches_posthook_payload(candidate, "submitted_plan"),
    )

    assert payload == {
        "items": [
            {
                "local_id": "dev1",
                "agent_name": "developer",
                "kind": "atomic",
                "payload": {"metadata": {"items": ["not-a-plan"]}},
            }
        ],
        "rationale": "ok",
    }

def test_posthook_ctx_propagates_live_team_plan_budget(monkeypatch):
    _, build_posthook_ctx = _make_context_builders("sbx-1")

    from team.runtime import registry as runtime_registry

    monkeypatch.setattr(
        runtime_registry,
        "get",
        lambda team_run_id: (
            SimpleNamespace(budgets=SimpleNamespace(max_plan_size=10))
            if team_run_id == "T1"
            else None
        ),
    )

    ctx = build_posthook_ctx(
        SimpleNamespace(name="submit_plan_agent"),
        {
            "final_text": '{"items":[{"agent_name":"developer","local_id":"dev1"}]}',
            "team_run_id": "T1",
            "work_item_id": "W1",
        },
    )

    assert ctx.tool_metadata["max_plan_size"] == 10


def test_query_ctx_seeds_repo_root_for_daytona_and_ci():
    build_query_ctx, _ = _make_context_builders("sbx-1", repo_dir="/testbed")
    ctx = build_query_ctx(
        SimpleNamespace(name="developer"),
        SimpleNamespace(
            id="TR1",
            sandbox_id="sbx-1",
            dispatcher=SimpleNamespace(
                artifact_store=SimpleNamespace(load=lambda _ref: None)
            ),
            budgets=None,
            project_context=None,
        ),
        WorkItem(
            id="W1",
            team_run_id="T1",
            agent_name="developer",
            status=WorkItemStatus.PENDING,
            kind=WorkItemKind.ATOMIC,
            payload={"prompt": "Fix it"},
        ),
    )

    assert ctx.tool_metadata.sandbox_id == "sbx-1"
    assert ctx.tool_metadata.daytona_cwd == "/testbed"
    assert ctx.tool_metadata["ci_workspace_root"] == "/testbed"
    assert ctx.tool_metadata["coordination_mode"] == "ultra"
    assert ctx.tool_metadata["require_declared_shell_outputs"] is True
    assert "Repo root inside the sandbox: /testbed" in ctx.user_message
    assert "Do not prepend guessed roots" in ctx.user_message


def test_query_ctx_injects_scope_packet_when_ci_is_available(monkeypatch):
    build_query_ctx, _ = _make_context_builders("sbx-1", repo_dir="/testbed")
    fake_ci = object()

    monkeypatch.setattr(sweevo_team_runner, "get_code_intelligence", lambda **_: fake_ci)
    monkeypatch.setattr(
        sweevo_team_runner,
        "build_scope_packet",
        lambda **_: {
            "coherence_token": "token-1",
            "freshness": "fresh",
            "scope_paths": ["src/module.py"],
        },
    )
    monkeypatch.setattr(
        sweevo_team_runner,
        "render_scope_packet",
        lambda packet: f"SCOPE {packet['coherence_token']}",
    )

    ctx = build_query_ctx(
        SimpleNamespace(name="developer"),
        SimpleNamespace(
            id="TR1",
            sandbox_id="sbx-1",
            user_request="Root prompt",
            dispatcher=SimpleNamespace(
                artifact_store=SimpleNamespace(load=lambda _ref: None)
            ),
            budgets=None,
            project_context=None,
        ),
        WorkItem(
            id="W1",
            team_run_id="T1",
            agent_name="developer",
            status=WorkItemStatus.PENDING,
            kind=WorkItemKind.ATOMIC,
            payload={"prompt": "Fix it", "files_to_edit": ["src/module.py"]},
        ),
    )

    assert ctx.tool_metadata["scope_packet"]["coherence_token"] == "token-1"
    assert ctx.tool_metadata["coherence_token"] == "token-1"
    assert ctx.tool_metadata["coordination_mode"] == "ultra"
    assert ctx.tool_metadata["require_declared_shell_outputs"] is True
    assert ctx.user_message.startswith("SCOPE token-1\n\n")


def test_root_prompt_points_to_skill_owned_workflow_policy():
    instance = SimpleNamespace(
        repo="pydantic/pydantic",
        instance_id="pydantic__pydantic_v2.6.0b1_v2.6.0",
        instance_id_swe="pydantic__pydantic_v2.6.0b1_v2.6.0",
        base_commit="deadbeef",
        start_version="2.6.0b1",
        end_version="2.6.0",
        docker_image="example/image:latest",
        test_cmds="pytest -q",
        problem_statement="- bullet\n" * 80,
        fail_to_pass=["tests/test_foo.py::test_bar"],
        pass_to_pass=["tests/test_foo.py::test_existing"],
    )

    prompt = _build_root_prompt(instance, "/repo")

    assert "The SWE-EVO test patch has already been applied inside the sandbox" in prompt
    assert "release notes are intentionally omitted from the root planner prompt" in prompt
    assert "Stable SWE-EVO workflow policy lives in the declared skills" in prompt
    assert "Recommended first-ready frontier cap" in prompt
    assert "submitted root plan must stay within 1-10 total tasks" in prompt
    assert "does not mean the whole submitted graph should stop at that many items" in prompt
    assert "do not hand the whole remaining surface to only the initial developers" in prompt
    assert "must still receive its own developer lane or expandable child planner" in prompt
    assert "must not inspect dependency/version metadata" in prompt
    assert "benchmark run log file under `.ephemeralos/benchmark-logs/`" in prompt


def test_agent_overrides_attach_sweevo_skills_without_prompt_duplication():
    sweevo_team_runner._register_team_builtins()
    instance = SimpleNamespace(
        repo="pydantic/pydantic",
        instance_id="pydantic__pydantic_v2.6.0b1_v2.6.0",
        instance_id_swe="pydantic__pydantic_v2.6.0b1_v2.6.0",
        start_version="2.6.0b1",
        end_version="2.6.0",
        docker_image="example/image:latest",
        test_cmds="pytest -q",
        problem_statement="- bullet\n" * 80,
        fail_to_pass=["tests/test_foo.py::test_bar"],
        pass_to_pass=["tests/test_foo.py::test_existing"],
    )

    overrides = _build_agent_overrides(instance)

    assert "system_prompt" not in overrides[TEAM_PLANNER]
    assert "sweevo-project-context" in overrides[TEAM_PLANNER]["skills"]
    assert "team_context" not in overrides[TEAM_PLANNER]["toolkits"]
    assert overrides[TEAM_PLANNER]["tool_call_limit"] == 100
    assert "system_prompt" not in overrides[DEVELOPER]
    assert "sweevo-project-context" in overrides[DEVELOPER]["skills"]
    assert "system_prompt" not in overrides[VALIDATOR]
    assert "sweevo-project-context" in overrides[VALIDATOR]["skills"]
    assert "verification-replan" in overrides[VALIDATOR]["skills"]
    assert "system_prompt" not in overrides[TEAM_REPLANNER]
    assert "sweevo-project-context" in overrides[TEAM_REPLANNER]["skills"]


def test_planner_runtime_limits_preserve_shared_agent_budget():
    large_single_target = SimpleNamespace(
        instance_id="large-one",
        instance_id_swe="large-one",
        repo="example/repo",
        start_version="1.0.0",
        end_version="1.0.1",
        docker_image="example/image:latest",
        test_cmds="pytest -q",
        fail_to_pass=["tests/test_foo.py::test_bar"],
        pass_to_pass=[],
        problem_statement="- bullet\n" * 80,
    )
    assert _derive_planner_runtime_limits(large_single_target) == {
        "tool_call_limit": 100,
    }

    medium_multi_target = SimpleNamespace(
        instance_id="medium-three",
        instance_id_swe="medium-three",
        repo="example/repo",
        start_version="1.0.0",
        end_version="1.0.1",
        docker_image="example/image:latest",
        test_cmds="pytest -q",
        fail_to_pass=["a", "b", "c"],
        pass_to_pass=[],
        problem_statement="- bullet\n" * 10,
    )
    assert _derive_planner_runtime_limits(medium_multi_target) == {
        "tool_call_limit": 100,
    }


def test_sweevo_budgets_cap_submitted_plan_size_at_ten():
    instance = SimpleNamespace(
        repo="pydantic/pydantic",
        instance_id="wide-plan",
        instance_id_swe="wide-plan",
        start_version="2.6.0b1",
        end_version="2.6.0",
        docker_image="example/image:latest",
        test_cmds="pytest -q",
        problem_statement="- bullet\n" * 80,
        fail_to_pass=[f"tests/test_{i}.py::test_case" for i in range(20)],
        pass_to_pass=["tests/test_guard.py::test_existing"],
    )

    budgets = _derive_sweevo_budgets(instance)

    assert budgets.max_plan_size == 10

def test_enforce_validation_evidence_requires_daytona_bash():
    with pytest.raises(RuntimeError, match="validator_missing_tool_evidence"):
        _enforce_validation_evidence(
            "validator",
            [ConversationMessage(role="assistant", content=[TextBlock(text="VERDICT: PASS")])],
        )

    _enforce_validation_evidence(
        "validator",
        [
            ConversationMessage(
                role="assistant",
                content=[
                    ToolUseBlock(
                        id="tc1",
                        name="daytona_bash",
                        input={"command": "pytest -q"},
                    )
                ],
            )
        ],
    )


def test_resume_sweevo_team_uses_default_executor_factory_signature(monkeypatch):
    instance = SimpleNamespace(
        repo="pydantic/pydantic",
        instance_id="pydantic__pydantic_v2.6.0b1_v2.6.0",
        instance_id_swe="pydantic__pydantic_v2.6.0b1_v2.6.0",
        start_version="2.6.0b1",
        end_version="2.6.0",
        docker_image="example/image:latest",
        test_cmds="pytest -q",
        problem_statement="- bullet\n" * 80,
        fail_to_pass=["tests/test_foo.py::test_bar"],
        pass_to_pass=["tests/test_foo.py::test_existing"],
    )
    fake_tr = SimpleNamespace(
        id="team-run-1",
        sandbox_id="sbx-1",
        session_id="sess-1",
        budgets=SimpleNamespace(),
        dispatcher=SimpleNamespace(graph={}, list_checkpoints=lambda: []),
        resume=AsyncMock(),
        wait=AsyncMock(),
    )

    monkeypatch.setattr(sweevo_team_runner, "_register_team_builtins", lambda: None)
    monkeypatch.setattr(sweevo_team_runner, "_build_benchmark_event_store", lambda **_: object())
    monkeypatch.setattr(
        sweevo_team_runner,
        "_prepare_benchmark_session",
        lambda **_: (SimpleNamespace(session_id="sess-1"), object()),
    )
    monkeypatch.setattr(sweevo_team_runner, "_build_agent_overrides", lambda _instance: {})
    monkeypatch.setattr(sweevo_team_runner, "_build_team_metrics", lambda: {})
    monkeypatch.setattr(sweevo_team_runner, "_emit_team_runtime_banner", lambda *args, **kwargs: None)
    monkeypatch.setattr(sweevo_team_runner, "_checkpoint_ids_from_store", lambda *args, **kwargs: [])
    def fake_resume_from(_store, _team_run_id, *, checkpoint_id=None):
        assert checkpoint_id is None
        return fake_tr

    monkeypatch.setattr(
        sweevo_team_runner.TeamRun,
        "resume_from",
        staticmethod(fake_resume_from),
    )

    seen_factory_calls: list[dict[str, object]] = []

    def fake_make_executor_factory(
        session_config,
        sandbox_id,
        printer,
        *,
        repo_dir="/testbed",
        team_metrics=None,
        agent_overrides=None,
    ):
        seen_factory_calls.append(
            {
                "session_config": session_config,
                "sandbox_id": sandbox_id,
                "printer": printer,
                "agent_overrides": agent_overrides,
            }
        )
        return "executor-factory"

    monkeypatch.setattr(sweevo_team_runner, "_make_executor_factory", fake_make_executor_factory)
    monkeypatch.setattr(
        sweevo_team_runner,
        "_finalize_team_result",
        lambda **_: {"status": "ok"},
    )

    result = asyncio.run(
        sweevo_team_runner.resume_sweevo_team(
            instance,
            "team-run-1",
        )
    )

    assert result == {"status": "ok"}
    assert seen_factory_calls and seen_factory_calls[0]["sandbox_id"] == "sbx-1"
    assert seen_factory_calls[0]["agent_overrides"] == {}
    fake_tr.resume.assert_awaited_once()


def test_make_runner_uses_agent_definition_limits(monkeypatch):
    captured_agents: list[SimpleNamespace] = []

    class _Tracker:
        def __init__(self) -> None:
            self.run_id = "run-1"

        def finish(self, **_: object) -> None:
            return None

    async def _fake_run(_prompt: str):
        if False:
            yield None

    def fake_spawn_agent(*_args, **_kwargs):
        agent = SimpleNamespace(
            query_context=SimpleNamespace(
                tool_metadata=ExecutionMetadata(session_config="cfg", sandbox_id="sbx-1"),
                run_id="",
                tool_call_limit=_kwargs["agent_def"].tool_call_limit,
                api_messages_snapshot=None,
            ),
            display_messages=[],
            total_usage=None,
            model="test-model",
            run=_fake_run,
        )
        captured_agents.append(agent)
        return agent

    monkeypatch.setattr(
        sweevo_team_runner,
        "AgentRunTracker",
        SimpleNamespace(create=lambda **_: _Tracker()),
    )
    monkeypatch.setattr(sweevo_team_runner, "spawn_agent", fake_spawn_agent)

    runner = _make_runner(
        session_config=SimpleNamespace(session_id="sess-1"),
        sandbox_id="sbx-1",
        printer=None,
        agent_overrides={"team_planner": {"tool_call_limit": 50}},
    )
    ctx = sweevo_team_runner.TeamAgentContext(
        user_message="Plan it",
        tool_metadata=ExecutionMetadata(team_run_id="TR1", work_item_id="W1"),
    )

    asyncio.run(
        runner(
            SimpleNamespace(
                name="team_planner",
                model_copy=lambda update: SimpleNamespace(name="team_planner", **update),
            ),
            ctx,
        )
    )

    assert captured_agents
    assert captured_agents[0].query_context.tool_metadata.agent_name == "team_planner"
    assert captured_agents[0].query_context.tool_call_limit == 50


def test_emit_dispatcher_dag_logs_graph_lines():
    lines: list[tuple[str, str]] = []
    printer = SimpleNamespace(raw_line=lambda agent, body: lines.append((agent, body)))
    root = WorkItem(
        id="root-1",
        team_run_id="TR1",
        agent_name="team_planner",
        status=WorkItemStatus.DONE,
        kind=WorkItemKind.EXPANDABLE,
        local_id="plan1",
        depth=0,
    )
    child = WorkItem(
        id="child-1",
        team_run_id="TR1",
        agent_name="developer",
        status=WorkItemStatus.READY,
        kind=WorkItemKind.ATOMIC,
        deps=["root-1"],
        local_id="dev1",
        depth=1,
    )
    team_run = SimpleNamespace(dispatcher=SimpleNamespace(graph={root.id: root, child.id: child}))

    _emit_dispatcher_dag(printer, team_run, trigger_agent="team_planner")

    assert lines[0] == ("team", "[dag] after=team_planner nodes=2")
    assert any("plan1 agent=team_planner" in body for _, body in lines[1:])
    assert any("dev1 agent=developer" in body and "deps=['plan1']" in body for _, body in lines[1:])
