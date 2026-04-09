from __future__ import annotations

import asyncio
from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest

from benchmarks.sweevo import team_runner as sweevo_team_runner
from benchmarks.sweevo.team_runner import (
    _build_sweevo_developer_runtime_prompt,
    _enforce_validation_evidence,
    _build_sweevo_planner_runtime_prompt,
    _emit_dispatcher_dag,
    _make_context_builders,
    _make_runner,
)
from message import ConversationMessage, TextBlock, ToolUseBlock
from team.models import WorkItem, WorkItemKind, WorkItemStatus
from tools.core.runtime import ExecutionMetadata


def test_posthook_ctx_prefers_final_text_over_wrapped_work_result():
    _, build_posthook_ctx = _make_context_builders("sbx-1")

    ctx = build_posthook_ctx(
        SimpleNamespace(name="submit_atlas_agent"),
        {
            "final_text": '{"chunks":[{"subsystem":"pydantic","brief":{"target_paths":["pydantic"]}}]}',
            "team_run_id": "T1",
            "work_item_id": "W1",
        },
    )

    assert ctx.user_message == (
        '{"chunks":[{"subsystem":"pydantic","brief":{"target_paths":["pydantic"]}}]}'
    )
    assert ctx.tool_metadata.team_run_id == "T1"
    assert ctx.tool_metadata.work_item_id == "W1"


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


def test_planner_runtime_prompt_avoids_timeout_and_budget_instructions():
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

    prompt = _build_sweevo_planner_runtime_prompt(instance)

    assert "timeout_seconds" not in prompt
    assert "hard stop" not in prompt
    assert "switch to scout-led exploration" in prompt
    assert "Treat planner-side ci_read_file as seed-only" in prompt
    assert "expandable child planner" in prompt
    assert "at most one additional direct code read" in prompt
    assert "exception, not a budget to spend by default" in prompt
    assert "Once you say or infer that you have enough context" in prompt


def test_developer_runtime_prompt_limits_post_failure_probes():
    prompt = _build_sweevo_developer_runtime_prompt()

    assert "at most one ad hoc python/bash probe" in prompt
    assert "trust the pytest failure as the source of truth" in prompt
    assert "Once a budget warning appears" in prompt


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
    monkeypatch.setattr(sweevo_team_runner, "_derive_atlas_parallelism", lambda *args, **kwargs: 1)
    monkeypatch.setattr(sweevo_team_runner, "_build_team_metrics", lambda: {})
    monkeypatch.setattr(sweevo_team_runner, "_emit_team_runtime_banner", lambda *args, **kwargs: None)
    monkeypatch.setattr(sweevo_team_runner, "_checkpoint_ids_from_store", lambda *args, **kwargs: [])
    monkeypatch.setattr(
        sweevo_team_runner.TeamRun,
        "resume_from",
        staticmethod(lambda _store, _team_run_id: fake_tr),
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
    seen_atlas_calls: list[dict[str, object]] = []

    def fake_make_atlas_scheduler_factory(*args, **kwargs):
        seen_atlas_calls.append(kwargs)
        return "atlas-factory"

    monkeypatch.setattr(sweevo_team_runner, "_make_atlas_scheduler_factory", fake_make_atlas_scheduler_factory)
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
    assert seen_atlas_calls
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
                max_turns=_kwargs["agent_def"].max_turns,
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
        agent_overrides={"team_planner": {"tool_call_limit": 50, "max_turns": 100}},
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
    assert captured_agents[0].query_context.max_turns == 100


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
