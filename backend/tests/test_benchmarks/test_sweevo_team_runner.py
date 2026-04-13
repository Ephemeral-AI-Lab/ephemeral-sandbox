from __future__ import annotations

import asyncio
from collections import Counter
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
    _checkpoint_repo_patch_from_store,
    _derive_planner_runtime_limits,
    _emit_dispatcher_dag,
    _make_context_builders,
    _make_runner,
)
from team.persistence.events import TeamRunEvent
from message.event_printer import MultiAgentEventPrinter
from message import ConversationMessage, TextBlock, ToolUseBlock
from message.stream_events import BackgroundTaskCompleted
from team.builtins import DEVELOPER, SCOUT, TEAM_PLANNER, TEAM_REPLANNER, VALIDATOR
from team.models import Task, TaskStatus
from tools.core.runtime import ExecutionMetadata


# ---------------------------------------------------------------------------
# Shared factories
# ---------------------------------------------------------------------------

def _pydantic_instance(**overrides) -> SimpleNamespace:
    """Return a minimal pydantic SWE-EVO instance namespace."""
    base = SimpleNamespace(
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
    for k, v in overrides.items():
        setattr(base, k, v)
    return base


def _fake_team_run(**overrides) -> SimpleNamespace:
    """Return a minimal fake TeamRun namespace."""
    base = SimpleNamespace(
        id="team-run-1",
        sandbox_id="sbx-1",
        session_id="sess-1",
        budgets=SimpleNamespace(),
        dispatcher=SimpleNamespace(graph={}, list_checkpoints=lambda: []),
        resume=AsyncMock(),
        wait=AsyncMock(),
    )
    for k, v in overrides.items():
        setattr(base, k, v)
    return base


def _patch_resume_sweevo_common(monkeypatch, *, checkpoint_records=None, checkpoint_patch="") -> None:
    """Apply the shared monkeypatches needed by resume_sweevo_team tests."""
    if checkpoint_records is None:
        checkpoint_records = []
    monkeypatch.setattr(sweevo_team_runner, "_register_team_builtins", lambda: None)
    monkeypatch.setattr("server.app_factory.ensure_runtime_stores_ready", lambda: object())
    monkeypatch.setattr(sweevo_team_runner, "_build_benchmark_event_store", lambda **_: object())
    monkeypatch.setattr(
        sweevo_team_runner,
        "_prepare_benchmark_session",
        lambda **_: (SimpleNamespace(session_id="sess-1"), object()),
    )
    monkeypatch.setattr(sweevo_team_runner, "_build_agent_overrides", lambda _instance: {})
    monkeypatch.setattr(sweevo_team_runner, "_build_team_metrics", lambda: {})
    monkeypatch.setattr(sweevo_team_runner, "_emit_team_runtime_banner", lambda *args, **kwargs: None)
    monkeypatch.setattr(
        sweevo_team_runner,
        "_checkpoint_records_from_store",
        lambda *args, **kwargs: checkpoint_records,
    )
    monkeypatch.setattr(
        sweevo_team_runner,
        "_checkpoint_repo_patch_from_store",
        lambda *args, **kwargs: checkpoint_patch,
    )
    monkeypatch.setattr(
        sweevo_team_runner,
        "_finalize_team_result",
        lambda **_: {"status": "ok"},
    )




@pytest.mark.asyncio
async def test_query_ctx_seeds_repo_root_for_daytona_and_ci():
    build_query_ctx = _make_context_builders("sbx-1", repo_dir="/testbed")
    ctx = await build_query_ctx(
        SimpleNamespace(name="developer"),
        SimpleNamespace(
            id="TR1",
            sandbox_id="sbx-1",
            dispatcher=SimpleNamespace(),
            task_center=SimpleNamespace(context_for=AsyncMock(return_value="")),
            budgets=None,
            budget_state=None,
            project_context=SimpleNamespace(repo_root="/testbed"),
            coordination_metadata={},
            user_request="Fix it",
            file_change_store=None,
        ),
        Task(
            id="W1",
            team_run_id="T1",
            agent_name="developer",
            status=TaskStatus.PENDING,
            task="Fix it",
        ),
    )

    assert ctx.tool_metadata.sandbox_id == "sbx-1"
    assert ctx.tool_metadata.daytona_cwd == "/testbed"
    assert ctx.tool_metadata["ci_workspace_root"] == "/testbed"
    assert ctx.tool_metadata["team_mode_enabled"] is True
    assert "Repo root inside the sandbox: /testbed" in ctx.user_message
    assert "Do not prepend guessed roots" in ctx.user_message



def test_root_prompt_points_to_skill_owned_workflow_policy():
    instance = _pydantic_instance()

    prompt = _build_root_prompt(instance, "/repo")

    assert "The SWE-EVO test patch has already been applied inside the sandbox" in prompt
    assert "This run is primarily evaluating the coordination behavior described in" in prompt
    assert "`docs/architecture/plan-a-team-coordination-redesign.md`" in prompt
    assert "let the declared skills own the detailed workflow policy" in prompt
    assert "Task Center, scout waves, scoped-path freshness, and recovery/replanning loop" in prompt
    assert "per-layer cap of 16 tasks as a budgeting guardrail" in prompt
    assert "`.ephemeralos/benchmark-logs/` only as supporting evidence" in prompt


def test_root_prompt_summarizes_large_pass_to_pass_guardrail():
    instance = _pydantic_instance(
        pass_to_pass=[f"tests/test_guard.py::test_case_{idx}" for idx in range(5000)],
    )

    prompt = _build_root_prompt(instance, "/repo")

    assert len(prompt.encode()) < 20000
    assert '"total_tests": 5000' in prompt
    assert '"sample_test_ids"' in prompt


def test_agent_overrides_attach_sweevo_skills_without_prompt_duplication():
    sweevo_team_runner._register_team_builtins()
    instance = _pydantic_instance()

    overrides = _build_agent_overrides(instance)

    assert "system_prompt" not in overrides[TEAM_PLANNER]
    assert "sweevo-project-context" in overrides[TEAM_PLANNER]["skills"]
    assert "context" in overrides[TEAM_PLANNER]["toolkits"]
    assert overrides[TEAM_PLANNER]["tool_call_limit"] == 100
    assert "system_prompt" not in overrides[DEVELOPER]
    assert "sweevo-project-context" in overrides[DEVELOPER]["skills"]
    assert overrides[DEVELOPER]["tool_call_limit"] == 50
    assert "system_prompt" not in overrides[SCOUT]
    assert "sweevo-project-context" in overrides[SCOUT]["skills"]
    assert overrides[SCOUT]["tool_call_limit"] == 50
    assert "system_prompt" not in overrides[VALIDATOR]
    assert "sweevo-project-context" in overrides[VALIDATOR]["skills"]
    assert "verification-replan" in overrides[VALIDATOR]["skills"]
    assert overrides[VALIDATOR]["tool_call_limit"] == 50
    assert "system_prompt" not in overrides[TEAM_REPLANNER]
    assert "sweevo-project-context" in overrides[TEAM_REPLANNER]["skills"]
    assert overrides[TEAM_REPLANNER]["tool_call_limit"] == 50


def test_planner_runtime_limits_preserve_shared_agent_budget():
    large_single_target = _pydantic_instance(
        instance_id="large-one",
        instance_id_swe="large-one",
        repo="example/repo",
        start_version="1.0.0",
        end_version="1.0.1",
        fail_to_pass=["tests/test_foo.py::test_bar"],
        pass_to_pass=[],
    )
    assert _derive_planner_runtime_limits(large_single_target) == {"tool_call_limit": 100}

    medium_multi_target = _pydantic_instance(
        instance_id="medium-three",
        instance_id_swe="medium-three",
        repo="example/repo",
        start_version="1.0.0",
        end_version="1.0.1",
        fail_to_pass=["a", "b", "c"],
        pass_to_pass=[],
        problem_statement="- bullet\n" * 10,
    )
    assert _derive_planner_runtime_limits(medium_multi_target) == {"tool_call_limit": 100}


def test_execution_runtime_limits_tighten_bounded_lanes():
    instance = _pydantic_instance(
        instance_id="exec-budget",
        instance_id_swe="exec-budget",
        repo="example/repo",
        start_version="1.0.0",
        end_version="1.0.1",
        fail_to_pass=["tests/test_foo.py::test_bar"],
        pass_to_pass=[],
    )

    assert sweevo_team_runner._derive_execution_runtime_limits(instance) == {
        "tool_call_limit": 50,
    }


def test_sweevo_budgets_follow_instance_size_ceiling():
    instance = _pydantic_instance(
        instance_id="wide-plan",
        instance_id_swe="wide-plan",
        fail_to_pass=[f"tests/test_{i}.py::test_case" for i in range(20)],
        pass_to_pass=["tests/test_guard.py::test_existing"],
    )

    budgets = _derive_sweevo_budgets(instance)

    assert budgets.max_plan_size == 16
    assert budgets.max_depth == 4


def test_checkpoint_repo_patch_from_store_returns_latest_matching_patch():
    store = SimpleNamespace(
        load_run=lambda _team_run_id: [
            TeamRunEvent(
                team_run_id="T1",
                kind="checkpoint_repo_state",
                data={"checkpoint_id": "cp-1", "repo_patch": "patch-a"},
            ),
            TeamRunEvent(
                team_run_id="T1",
                kind="checkpoint_repo_state",
                data={"checkpoint_id": "cp-2", "repo_patch": "patch-b"},
            ),
            TeamRunEvent(
                team_run_id="T1",
                kind="checkpoint_repo_state",
                data={"checkpoint_id": "cp-1", "repo_patch": "patch-a2"},
            ),
        ]
    )

    assert _checkpoint_repo_patch_from_store(store, "T1", "cp-1") == "patch-a2"
    assert _checkpoint_repo_patch_from_store(store, "T1", "cp-2") == "patch-b"
    assert _checkpoint_repo_patch_from_store(store, "T1", "missing") == ""

def test_enforce_validation_evidence_requires_daytona_codeact():
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
                        name="daytona_codeact",
                        input={"code": "shell('pytest -q')"},
                    )
                ],
            )
        ],
    )


def test_resume_sweevo_team_uses_default_executor_factory_signature(monkeypatch):
    instance = _pydantic_instance()
    fake_tr = _fake_team_run()

    _patch_resume_sweevo_common(monkeypatch)

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

    result = asyncio.run(
        sweevo_team_runner.resume_sweevo_team(
            instance,
            "team-run-1",
        )
    )

    assert result == {"status": "ok"}
    assert seen_factory_calls and seen_factory_calls[0]["sandbox_id"] == "sbx-1"
    assert seen_factory_calls[0]["agent_overrides"] == {}
    fake_tr.resume.assert_awaited_once_with(
        executor_factory="executor-factory",
        num_executors=sweevo_team_runner._DEFAULT_NUM_EXECUTORS,
        resumed_from="team-run-1",
        resumed_from_checkpoint=None,
    )


def test_resume_sweevo_team_restores_checkpoint_repo_patch(monkeypatch):
    instance = _pydantic_instance()
    fake_tr = _fake_team_run()

    _patch_resume_sweevo_common(
        monkeypatch,
        checkpoint_records=[{"id": "cp-1", "label": "durable:complete:developer:dev1", "sequence": 1}],
        checkpoint_patch="diff --git a/x b/x",
    )
    monkeypatch.setattr(
        sweevo_team_runner.TeamRun,
        "resume_from",
        staticmethod(lambda *_args, **_kwargs: fake_tr),
    )
    monkeypatch.setattr(sweevo_team_runner, "setup_sweevo_sandbox", AsyncMock())
    monkeypatch.setattr(sweevo_team_runner, "ensure_sweevo_test_patch", AsyncMock())
    monkeypatch.setattr(sweevo_team_runner, "apply_sweevo_repo_patch", AsyncMock())
    monkeypatch.setattr(sweevo_team_runner, "_make_executor_factory", lambda *args, **kwargs: "executor-factory")

    result = asyncio.run(
        sweevo_team_runner.resume_sweevo_team(
            instance,
            "team-run-1",
            checkpoint_id="cp-1",
        )
    )

    assert result == {"status": "ok"}
    sweevo_team_runner.setup_sweevo_sandbox.assert_awaited_once_with(instance, "sbx-1", "/testbed")
    sweevo_team_runner.ensure_sweevo_test_patch.assert_awaited_once_with(
        instance, "sbx-1", "/testbed"
    )
    sweevo_team_runner.apply_sweevo_repo_patch.assert_awaited_once_with(
        "sbx-1",
        "diff --git a/x b/x",
        "/testbed",
    )
    fake_tr.resume.assert_awaited_once()


def test_resume_sweevo_team_reapplies_benchmark_patch_when_checkpoint_patch_missing(monkeypatch):
    instance = _pydantic_instance(
        repo="dask/dask",
        instance_id="dask__dask_2023.3.2_2023.4.0",
        instance_id_swe="dask__dask_2023.3.2_2023.4.0",
        start_version="2023.3.2",
        end_version="2023.4.0",
        fail_to_pass=["tests/test_groupby.py::test_value_counts"],
        pass_to_pass=["tests/test_groupby.py::test_existing"],
    )
    fake_tr = _fake_team_run()

    _patch_resume_sweevo_common(
        monkeypatch,
        checkpoint_records=[{"id": "cp-1", "label": "durable:complete:validator:val1", "sequence": 1}],
        checkpoint_patch="",
    )
    monkeypatch.setattr(
        sweevo_team_runner.TeamRun,
        "resume_from",
        staticmethod(lambda *_args, **_kwargs: fake_tr),
    )
    monkeypatch.setattr(sweevo_team_runner, "setup_sweevo_sandbox", AsyncMock())
    monkeypatch.setattr(sweevo_team_runner, "ensure_sweevo_test_patch", AsyncMock())
    monkeypatch.setattr(sweevo_team_runner, "apply_sweevo_repo_patch", AsyncMock())
    monkeypatch.setattr(sweevo_team_runner, "_make_executor_factory", lambda *args, **kwargs: "executor-factory")

    result = asyncio.run(
        sweevo_team_runner.resume_sweevo_team(
            instance,
            "team-run-1",
            checkpoint_id="cp-1",
        )
    )

    assert result == {"status": "ok"}
    sweevo_team_runner.setup_sweevo_sandbox.assert_awaited_once_with(instance, "sbx-1", "/testbed")
    sweevo_team_runner.ensure_sweevo_test_patch.assert_awaited_once_with(
        instance, "sbx-1", "/testbed"
    )
    sweevo_team_runner.apply_sweevo_repo_patch.assert_not_awaited()
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


def test_make_runner_persists_full_compaction_delta(monkeypatch):
    tracker_finishes: list[dict[str, object]] = []
    printed: list[tuple[str, str]] = []

    class _Tracker:
        run_id = "run-1"

        def finish(self, **kwargs: object) -> None:
            tracker_finishes.append(kwargs)

    async def _fake_run(_prompt: str):
        state.compacted = 3
        query_context.tool_calls_used = 4
        if False:
            yield None

    state = SimpleNamespace(compacted=1)
    query_context = SimpleNamespace(
        tool_metadata=ExecutionMetadata(session_config="cfg", sandbox_id="sbx-1"),
        run_id="",
        tool_call_limit=10,
        tool_calls_used=0,
        session_state=state,
        api_messages_snapshot=["snapshot"],
    )
    agent = SimpleNamespace(
        query_context=query_context,
        display_messages=[],
        total_usage=SimpleNamespace(input_tokens=12, output_tokens=8),
        model="test-model",
        run=_fake_run,
    )

    monkeypatch.setattr(
        sweevo_team_runner,
        "AgentRunTracker",
        SimpleNamespace(create=lambda **_: _Tracker()),
    )
    monkeypatch.setattr(sweevo_team_runner, "spawn_agent", lambda *_args, **_kwargs: agent)
    monkeypatch.setattr(sweevo_team_runner, "_estimate_final_context", lambda _messages: 321)
    monkeypatch.setattr(sweevo_team_runner, "_persist_benchmark_session", lambda **_: None)

    runner = _make_runner(
        session_config=SimpleNamespace(session_id="sess-1"),
        sandbox_id="sbx-1",
        printer=SimpleNamespace(
            raw_line=lambda who, body: printed.append((who, body)),
            emit=lambda _event: None,
        ),
    )
    ctx = sweevo_team_runner.TeamAgentContext(
        user_message="Ship it",
        tool_metadata=ExecutionMetadata(team_run_id="TR1", work_item_id="W1"),
    )

    asyncio.run(
        runner(
            SimpleNamespace(name="developer", model_copy=lambda update: SimpleNamespace(name="developer", **update)),
            ctx,
        )
    )

    assert tracker_finishes
    response = tracker_finishes[0]["response"]
    assert isinstance(response, dict)
    assert response["tool_calls_used"] == 4
    assert response["tool_call_limit"] == 10
    assert response["final_context_tokens"] == 321
    assert response["compactions_added"] == 2
    assert response["compacted"] == 3
    assert any(
        body == "[usage] prompt=12 completion=8 total=20 tool_calls=4/10 final_context=321 compactions=+2(total=3)"
        for _, body in printed
    )


def test_finalize_team_result_surfaces_retry_replan_and_checkpoint_metadata(monkeypatch):
    printed: list[tuple[str, str]] = []
    fake_usage_store = SimpleNamespace(
        is_ready=True,
        get_session_usage=lambda _session_id: {
            "prompt_tokens": 11,
            "completion_tokens": 7,
            "total_tokens": 18,
            "run_count": 2,
        },
        get_usage_by_model=lambda _session_id: [{"model_id": "test-model", "total_tokens": 18}],
    )
    monkeypatch.setattr("server.app_factory.usage_store", fake_usage_store, raising=False)

    result = sweevo_team_runner._finalize_team_result(
        tr=SimpleNamespace(
            id="TR1",
            status=sweevo_team_runner.TeamRunStatus.SUCCEEDED,
            sandbox_id="sbx-1",
            budget_state=SimpleNamespace(replans_used=2),
            dispatcher=SimpleNamespace(
                graph={
                    "A": Task(
                        id="A",
                        team_run_id="TR1",
                        agent_name="developer",
                        status=TaskStatus.DONE,
                        task="task",
                        retry_count=1,
                    ),
                    "B": Task(
                        id="B",
                        team_run_id="TR1",
                        agent_name="validator",
                        status=TaskStatus.DONE,
                        task="task",
                        retry_count=2,
                        depth=1,
                    ),
                },
                list_checkpoints=lambda: [],
            ),
        ),
        session_config=SimpleNamespace(session_id="sess-1"),
        team_metrics={
            "agent_runs": 4,
            "agent_counts": Counter({"developer": 2, "validator": 2}),
            "checkpoint_ids": [],
            "checkpoints": [],
        },
        budgets=SimpleNamespace(
            max_tasks=10,
            max_depth=5,
            max_plan_size=6,
        ),
        printer=SimpleNamespace(raw_line=lambda who, body: printed.append((who, body))),
        checkpoint_records=[
            {"id": "cp-1", "label": "planner:W1", "sequence": 1},
            {"id": "cp-2", "label": "durable:complete:developer:A", "sequence": 2},
        ],
        resumed_from="TR0",
        resumed_from_checkpoint="cp-1",
    )

    assert result["retry_count_total"] == 3
    assert result["replans_used"] == 2
    assert result["checkpoints"][-1]["label"] == "durable:complete:developer:A"
    assert result["latest_checkpoint_id"] == "cp-2"
    assert result["latest_checkpoint_label"] == "durable:complete:developer:A"
    assert any(
        body == "[team_stats] tasks=2 max_depth=1 agent_runs=4 checkpoints=2 retries=3 replans=2"
        for _, body in printed
    )


def test_emit_dispatcher_dag_logs_graph_lines():
    lines: list[tuple[str, str]] = []
    printer = SimpleNamespace(raw_line=lambda agent, body: lines.append((agent, body)))
    root = Task(
        id="root-1",
        team_run_id="TR1",
        agent_name="team_planner",
        status=TaskStatus.DONE,
        task="plan",
        depth=0,
    )
    child = Task(
        id="child-1",
        team_run_id="TR1",
        agent_name="developer",
        status=TaskStatus.READY,
        task="child task",
        deps=["root-1"],
        depth=1,
    )
    team_run = SimpleNamespace(dispatcher=SimpleNamespace(graph={root.id: root, child.id: child}))

    _emit_dispatcher_dag(printer, team_run, trigger_agent="team_planner")

    assert lines[0] == ("team", "[dag] after=team_planner nodes=2")
    assert any("root-1 agent=team_planner" in body for _, body in lines[1:])
    assert any("child-1 agent=developer" in body and "deps=['root-1']" in body for _, body in lines[1:])
