from __future__ import annotations

import asyncio
from collections import Counter
import json
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest

from benchmarks.sweevo import team_runner as sweevo_team_runner
from benchmarks.sweevo.team_runner import (
    _derive_sweevo_budgets,
    _enforce_validation_evidence,
    _build_agent_overrides,
    _make_external_hook_emitter,
    _build_root_prompt,
    _checkpoint_repo_patch_from_store,
    _derive_planner_runtime_limits,
    _emit_dispatcher_dag,
    _make_context_builders,
    _make_runner,
)
from agents.types import AgentDefinition
from engine.core.query import QueryExitReason
from team.persistence.events import TeamRunEvent
from message import ConversationMessage, TextBlock, ToolUseBlock
from team.builtins import (
    DEVELOPER,
    ROOT_PLANNER,
    SCOUT,
    TEAM_PLANNER,
    TEAM_REPLANNER,
    VALIDATOR,
)
from team.models import Task, TaskStatus, TeamDefinition, TeamRunStatus
from team.task_context_builder import UserPromptContextParts
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
        task_center=SimpleNamespace(graph={}, list_checkpoints=lambda: []),
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


def test_load_or_create_team_definition_uses_requested_db_name(monkeypatch):
    target = SimpleNamespace(name="sweevo-team-glm5.1")
    captured: dict[str, object] = {}

    class _Store:
        def initialize(self, session_factory):
            captured["session_factory"] = session_factory

        def seed_builtin(self, _defn):
            raise AssertionError("custom DB team should not seed builtin definition")

        def get_by_name(self, name):
            captured["team_name"] = name
            return target

    monkeypatch.setattr(sweevo_team_runner, "TeamDefinitionStore", _Store)
    monkeypatch.setattr("team.registry.get_team_definition", lambda _name: None)

    session_factory = object()
    result = sweevo_team_runner._load_or_create_team_definition(
        session_factory,
        team_name="sweevo-team-glm5.1",
    )

    assert result is target
    assert captured == {
        "session_factory": session_factory,
        "team_name": "sweevo-team-glm5.1",
    }


def test_load_or_create_team_definition_uses_current_builtin_over_stale_db(monkeypatch):
    builtin = TeamDefinition(
        id="builtin-sweevo",
        name="sweevo_benchmark",
        description="current builtin team",
        entry_planner=ROOT_PLANNER,
        roster={
            "planner": [ROOT_PLANNER, TEAM_PLANNER],
            "developer": [DEVELOPER],
            "reviewer": [VALIDATOR],
            "explorer": [SCOUT],
        },
        terminal_tools={"note_taker": {"submit_task_note"}},
    )
    stale_db = TeamDefinition(
        id="stale-sweevo",
        name="sweevo_benchmark",
        description="old builtin team",
        entry_planner=TEAM_PLANNER,
        roster={
            "planner": [TEAM_PLANNER],
            "developer": [DEVELOPER],
            "reviewer": [VALIDATOR],
            "explorer": [SCOUT],
        },
        terminal_tools={"note_taker": {"submit_task_note"}},
    )
    captured: dict[str, object] = {}

    class _Store:
        def initialize(self, session_factory):
            captured["session_factory"] = session_factory

        def seed_builtin(self, defn):
            captured["seeded"] = defn
            return stale_db

        def get_by_name(self, name):
            raise AssertionError(f"built-in registry should handle {name}")

    monkeypatch.setattr(sweevo_team_runner, "TeamDefinitionStore", _Store)
    monkeypatch.setattr("team.registry.get_team_definition", lambda _name: builtin)

    session_factory = object()
    result = sweevo_team_runner._load_or_create_team_definition(
        session_factory,
        team_name="sweevo_benchmark",
    )

    assert result is builtin
    assert captured == {
        "session_factory": session_factory,
        "seeded": builtin,
    }




@pytest.mark.asyncio
async def test_query_ctx_seeds_repo_root_for_daytona_and_ci():
    build_query_ctx = _make_context_builders("sbx-1", repo_dir="/testbed")
    template_context_for = AsyncMock(return_value=UserPromptContextParts(task_spec="Fix it"))
    ctx = await build_query_ctx(
        SimpleNamespace(name="developer", role="developer"),
            SimpleNamespace(
                id="TR1",
                sandbox_id="sbx-1",
                task_center=SimpleNamespace(
                    context=SimpleNamespace(
                        context_for=AsyncMock(return_value=""),
                        template_context_for=template_context_for,
                    ),
                    notes=SimpleNamespace(context_for=AsyncMock(return_value="")),
                    graph={},
                ),
                budgets=None,
                budget_state=None,
                project_context=SimpleNamespace(repo_root="/testbed"),
            coordination_metadata={},
            user_request="Fix it",
            arbiter=None,
        ),
        Task(
            id="W1",
            team_run_id="T1",
            agent_name="developer",
            status=TaskStatus.PENDING,
            objective="Fix it",
        ),
    )

    assert ctx.tool_metadata.sandbox_id == "sbx-1"
    assert ctx.tool_metadata["exec_cwd"] == "/testbed"
    assert ctx.tool_metadata["ci_workspace_root"] == "/testbed"
    assert ctx.tool_metadata["role"] == "developer"
    assert ctx.user_message.startswith("Please read the following sections")
    assert "- submit_task_success:" in ctx.user_message
    assert "Fix it" in ctx.user_message
    assert "Repo root inside the sandbox: /testbed" not in ctx.user_message
    assert "Do not prepend guessed roots" not in ctx.user_message



def test_root_prompt_includes_instance_essentials():
    """Root prompt carries only instance-specific info — agent skills/system
    prompts (loaded from DB) supply the rest of the workflow policy."""
    instance = _pydantic_instance()

    prompt = _build_root_prompt(instance, "/repo")

    assert instance.repo in prompt
    assert instance.base_commit in prompt
    assert instance.test_cmds in prompt
    assert "fail-to-pass" in prompt.lower()
    assert json.dumps(instance.fail_to_pass, indent=2) in prompt
    assert "/repo" in prompt


def test_root_prompt_stays_compact_for_large_pass_to_pass():
    instance = _pydantic_instance(
        pass_to_pass=[f"tests/test_guard.py::test_case_{idx}" for idx in range(5000)],
    )

    prompt = _build_root_prompt(instance, "/repo")

    assert len(prompt.encode()) < 20000
    assert "Pass-To-Pass count: 5000" in prompt


def test_external_hook_emitter_writes_raw_and_structured_logs(tmp_path):
    structured_log = tmp_path / "events.jsonl"
    lines: list[tuple[str, str]] = []

    class _Printer:
        def raw_line(self, agent_name: str, line: str) -> None:
            lines.append((agent_name, line))

    emitter = _make_external_hook_emitter(
        printer=_Printer(),
        team_metrics={"structured_log_path": str(structured_log)},
    )
    emitter({
        "event": "external_hook",
        "hook": "tc_note",
        "team_run_id": "run-1",
        "work_item_id": "task-1",
        "trigger": "turn",
        "status": "completed",
    })

    payload = json.loads(structured_log.read_text(encoding="utf-8").strip())
    assert payload["hook"] == "tc_note"
    assert payload["trigger"] == "turn"
    assert lines == [
        (
            "team",
            "[external_hook] tc_note task=task-1 trigger=turn status=completed",
        )
    ]


def test_agent_overrides_attach_validator_skill_without_prompt_duplication():
    sweevo_team_runner._register_team_builtins()
    instance = _pydantic_instance()

    overrides = _build_agent_overrides(instance)

    assert "system_prompt" not in overrides[TEAM_PLANNER]
    assert "task_center" in overrides[TEAM_PLANNER]["toolkits"]
    assert overrides[TEAM_PLANNER]["tool_call_limit"] == 100
    assert "system_prompt" not in overrides[DEVELOPER]
    assert overrides[DEVELOPER]["tool_call_limit"] == 50
    assert "system_prompt" not in overrides[SCOUT]
    assert overrides[SCOUT]["tool_call_limit"] == 50
    assert "system_prompt" not in overrides[VALIDATOR]
    assert overrides[VALIDATOR]["tool_call_limit"] == 50
    assert "system_prompt" not in overrides[TEAM_REPLANNER]
    assert overrides[TEAM_REPLANNER]["tool_call_limit"] == 50


@pytest.mark.asyncio
async def test_root_planner_runtime_prompt_hides_legacy_plan_tool_name():
    build_query_ctx = _make_context_builders("sbx-1", repo_dir="/testbed")
    template_context_for = AsyncMock(
        return_value=UserPromptContextParts(task_spec="Root planning task")
    )
    ctx = await build_query_ctx(
        SimpleNamespace(name="root_planner", role="planner"),
            SimpleNamespace(
                id="TR1",
                sandbox_id="sbx-1",
                user_request="Root plan the repo.",
                root_task_id="W1",
                task_center=SimpleNamespace(
                    context=SimpleNamespace(
                        context_for=AsyncMock(return_value=""),
                        template_context_for=template_context_for,
                    ),
                    notes=SimpleNamespace(context_for=AsyncMock(return_value="")),
                    graph={},
                ),
                budgets=None,
                budget_state=None,
                project_context=SimpleNamespace(repo_root="/testbed"),
            coordination_metadata={},
            arbiter=None,
        ),
        Task(
            id="W1",
            team_run_id="T1",
            agent_name="root_planner",
            status=TaskStatus.PENDING,
            objective="Root planning task",
            depth=0,
        ),
    )

    assert ctx.user_message.startswith("Please read the following sections")
    assert "- submit_plan:" in ctx.user_message
    assert "## Available Agents" not in ctx.user_message
    assert ctx.user_message.count("Root plan the repo.") == 1
    legacy_tool_name = "submit_" + "task_plan"
    assert legacy_tool_name not in ctx.user_message


def test_build_benchmark_event_store_uses_project_local_team_run_dir(monkeypatch):
    captured: dict[str, object] = {}
    sentinel = object()

    def fake_build_default_store(*, base_dir):
        captured["base_dir"] = base_dir
        return sentinel

    monkeypatch.setattr(sweevo_team_runner, "build_default_store", fake_build_default_store)

    store = sweevo_team_runner._build_benchmark_event_store()

    assert store is sentinel
    assert captured["base_dir"] == sweevo_team_runner._benchmark_team_run_dir()


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
    assert budgets.max_depth == 6


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

def test_enforce_validation_evidence_requires_daytona_shell():
    def _state(messages):
        return SimpleNamespace(
            defn=SimpleNamespace(name="validator"),
            agent=SimpleNamespace(display_messages=messages),
        )

    with pytest.raises(RuntimeError, match="validator_missing_tool_evidence"):
        _enforce_validation_evidence(
            _state([ConversationMessage(role="assistant", content=[TextBlock(text="VERDICT: PASS")])])
        )

    _enforce_validation_evidence(
        _state([
            ConversationMessage(
                role="assistant",
                content=[
                    ToolUseBlock(
                        id="tc1",
                        name="daytona_shell",
                        input={"code": "shell('pytest -q')"},
                    )
                ],
            )
        ])
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
                exit_reason=QueryExitReason.TOOL_STOP,
                terminal_tools=set(),
            ),
            display_messages=[],
            total_usage=None,
            model="test-model",
            run=_fake_run,
        )
        captured_agents.append(agent)
        return agent

    monkeypatch.setattr("team.runtime.runner.AgentRunTracker", SimpleNamespace(create=lambda **_: _Tracker()))
    monkeypatch.setattr("team.runtime.runner.spawn_agent", fake_spawn_agent)

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
        exit_reason=QueryExitReason.TOOL_STOP,
        terminal_tools=set(),
    )
    agent = SimpleNamespace(
        query_context=query_context,
        display_messages=[],
        total_usage=SimpleNamespace(input_tokens=12, output_tokens=8),
        model="test-model",
        run=_fake_run,
    )

    monkeypatch.setattr("team.runtime.runner.AgentRunTracker", SimpleNamespace(create=lambda **_: _Tracker()))
    monkeypatch.setattr("team.runtime.runner.spawn_agent", lambda *_args, **_kwargs: agent)
    monkeypatch.setattr("team.runtime.telemetry.estimate_final_context", lambda _messages: 321)
    monkeypatch.setattr("team.runtime.telemetry.persist_session_snapshot", lambda **_: None)

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
            SimpleNamespace(name="developer", allowed_triggers=["tc_note"], model_copy=lambda update: SimpleNamespace(name="developer", allowed_triggers=["tc_note"], **update)),
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


def test_make_runner_persists_work_result(monkeypatch):
    class _Tracker:
        run_id = "run-1"

        def finish(self, **_: object) -> None:
            return None

    final_text = (
        '{"tasks":[{"id":"dev-1","objective":"Fix auth","agent":"developer","deps":[],'
        '"scope_paths":["src/auth"]}],"rationale":"split by owner"}'
    )

    async def _fake_run(_prompt: str):
        agent.display_messages = [
            ConversationMessage(role="assistant", content=[TextBlock(text=final_text)])
        ]
        if False:
            yield None

    query_context = SimpleNamespace(
        tool_metadata=ExecutionMetadata(session_config="cfg", sandbox_id="sbx-1"),
        run_id="",
        tool_call_limit=10,
        tool_calls_used=0,
        session_state=None,
        api_messages_snapshot=[],
        exit_reason=QueryExitReason.TOOL_STOP,
        terminal_tools=set(),
    )
    agent = SimpleNamespace(
        query_context=query_context,
        display_messages=[],
        total_usage=SimpleNamespace(input_tokens=0, output_tokens=0),
        model="test-model",
        run=_fake_run,
    )

    monkeypatch.setattr("team.runtime.runner.AgentRunTracker", SimpleNamespace(create=lambda **_: _Tracker()))
    monkeypatch.setattr("team.runtime.runner.spawn_agent", lambda *_args, **_kwargs: agent)
    monkeypatch.setattr("team.runtime.telemetry.estimate_final_context", lambda _messages: 0)
    monkeypatch.setattr("team.runtime.telemetry.persist_session_snapshot", lambda **_: None)
    monkeypatch.setattr("team.runtime.registry.get", lambda _team_run_id: None)

    runner = _make_runner(
        session_config=SimpleNamespace(session_id="sess-1"),
        sandbox_id="sbx-1",
        printer=None,
    )
    ctx = sweevo_team_runner.TeamAgentContext(
        user_message="Plan it",
        tool_metadata=ExecutionMetadata(team_run_id="TR1", work_item_id="W1"),
    )

    asyncio.run(
        runner(
            SimpleNamespace(name="team_planner", model_copy=lambda update: SimpleNamespace(name="team_planner", **update)),
            ctx,
        )
    )

    assert ctx.tool_metadata["work_result"] == final_text


def test_make_runner_writes_agent_run_log_artifact(monkeypatch, tmp_path: Path):
    class _Tracker:
        run_id = "run-1"

        def finish(self, **_: object) -> None:
            return None

    async def _fake_run(prompt: str):
        assert prompt == "Fix it"
        agent.display_messages = [
            ConversationMessage(
                role="assistant",
                content=[TextBlock(text="Implemented the requested fix.")],
            )
        ]
        if False:
            yield None

    query_context = SimpleNamespace(
        tool_metadata=ExecutionMetadata(session_config="cfg", sandbox_id="sbx-1"),
        run_id="",
        tool_call_limit=10,
        tool_calls_used=3,
        session_state=None,
        api_messages_snapshot=[
            ConversationMessage(
                role="user",
                content=[TextBlock(text="Compacted API prompt")],
            )
        ],
        exit_reason=QueryExitReason.TOOL_STOP,
        terminal_tools=set(),
        system_prompt="Runtime system prompt",
    )
    agent = SimpleNamespace(
        query_context=query_context,
        display_messages=[],
        total_usage=SimpleNamespace(input_tokens=101, output_tokens=22),
        model="test-model",
        run=_fake_run,
    )

    monkeypatch.setattr("team.runtime.runner.AgentRunTracker", SimpleNamespace(create=lambda **_: _Tracker()))
    monkeypatch.setattr("team.runtime.runner.spawn_agent", lambda *_args, **_kwargs: agent)
    monkeypatch.setattr("team.runtime.telemetry.estimate_final_context", lambda _messages: 456)
    monkeypatch.setattr("team.runtime.telemetry.persist_session_snapshot", lambda **_: None)
    monkeypatch.setattr("team.runtime.registry.get", lambda _team_run_id: None)

    runner = _make_runner(
        session_config=SimpleNamespace(session_id="sess-1"),
        sandbox_id="sbx-1",
        printer=None,
        team_metrics={"agent_run_log_dir": str(tmp_path)},
    )
    ctx = sweevo_team_runner.TeamAgentContext(
        user_message="Fix it",
        tool_metadata=ExecutionMetadata(team_run_id="TR1", work_item_id="W1"),
    )

    asyncio.run(
        runner(
            AgentDefinition(
                name="developer",
                description="Developer",
                system_prompt="Definition prompt",
                model="test-model",
                role="developer",
                skills=[],
                tool_call_limit=10,
            ),
            ctx,
        )
    )

    files = list(tmp_path.glob("*.json"))
    assert len(files) == 1
    payload = json.loads(files[0].read_text(encoding="utf-8"))
    assert payload["team_run_id"] == "TR1"
    assert payload["work_item_id"] == "W1"
    assert payload["agent_run_id"] == "run-1"
    assert payload["agent_definition"]["name"] == "developer"
    assert payload["system_prompt"] == "Runtime system prompt"
    assert payload["user_prompt"] == "Fix it"
    assert payload["assistant_response"] == "Implemented the requested fix."
    assert payload["usage"] == {
        "prompt_tokens": 101,
        "completion_tokens": 22,
        "total_tokens": 123,
    }
    assert payload["token_trackers"]["tool_calls_used"] == 3
    assert payload["token_trackers"]["final_context_tokens"] == 456
    assert payload["display_messages"][-1]["role"] == "assistant"
    assert payload["display_messages"][-1]["content"][0]["text"] == "Implemented the requested fix."
    assert payload["api_messages"][-1]["role"] == "user"
    assert payload["api_messages"][-1]["content"][0]["text"] == "Compacted API prompt"


def test_make_runner_marks_cancelled_agent_run_log(monkeypatch, tmp_path: Path):
    finished_statuses: list[str] = []

    class _Tracker:
        run_id = "run-cancelled"

        def finish(self, **kwargs: object) -> None:
            finished_statuses.append(str(kwargs.get("status")))

    async def _fake_run(_prompt: str):
        agent.display_messages = [
            ConversationMessage(
                role="assistant",
                content=[TextBlock(text="Still working when run cancelled.")],
            )
        ]
        raise asyncio.CancelledError()
        if False:
            yield None

    query_context = SimpleNamespace(
        tool_metadata=ExecutionMetadata(session_config="cfg", sandbox_id="sbx-1"),
        run_id="",
        tool_call_limit=10,
        tool_calls_used=2,
        session_state=None,
        api_messages_snapshot=[],
        exit_reason=None,
        terminal_tools={"submit_task_success"},
        system_prompt="Runtime system prompt",
    )
    agent = SimpleNamespace(
        query_context=query_context,
        display_messages=[],
        total_usage=SimpleNamespace(input_tokens=7, output_tokens=3),
        model="test-model",
        run=_fake_run,
    )

    monkeypatch.setattr("team.runtime.runner.AgentRunTracker", SimpleNamespace(create=lambda **_: _Tracker()))
    monkeypatch.setattr("team.runtime.runner.spawn_agent", lambda *_args, **_kwargs: agent)
    monkeypatch.setattr("team.runtime.telemetry.estimate_final_context", lambda _messages: 0)
    monkeypatch.setattr("team.runtime.telemetry.persist_session_snapshot", lambda **_: None)
    monkeypatch.setattr("team.runtime.registry.get", lambda _team_run_id: None)

    runner = _make_runner(
        session_config=SimpleNamespace(session_id="sess-1"),
        sandbox_id="sbx-1",
        printer=None,
        team_metrics={"agent_run_log_dir": str(tmp_path)},
    )
    ctx = sweevo_team_runner.TeamAgentContext(
        user_message="Fix it",
        tool_metadata=ExecutionMetadata(team_run_id="TR1", work_item_id="W1"),
    )

    with pytest.raises(asyncio.CancelledError):
        asyncio.run(
            runner(
                AgentDefinition(
                    name="developer",
                    description="Developer",
                    system_prompt="Definition prompt",
                    model="test-model",
                    role="developer",
                    skills=[],
                    tool_call_limit=10,
                ),
                ctx,
            )
        )

    assert finished_statuses == ["cancelled"]
    files = list(tmp_path.glob("*.json"))
    assert len(files) == 1
    payload = json.loads(files[0].read_text(encoding="utf-8"))
    assert payload["status"] == "cancelled"
    assert payload["assistant_response"] == "Still working when run cancelled."


def test_make_runner_logs_tc_note_external_hook(monkeypatch, tmp_path: Path):
    structured_log_path = tmp_path / "benchmark.events.jsonl"
    raw_lines: list[tuple[str, str]] = []
    checkpoint_calls: list[dict[str, object]] = []

    class _Tracker:
        run_id = "run-1"

        def finish(self, **_: object) -> None:
            return None

    class _TaskCenter:
        def __init__(self) -> None:
            self._triggered = False
            self.activity = self

        def tick(self, _task_id: str) -> None:
            return None

        def should_take_note(self, _task_id: str) -> str | None:
            if self._triggered:
                return None
            self._triggered = True
            return "turn"

        async def check(
            self,
            task_id: str,
            *,
            snapshot: list[dict[str, object]] | None = None,
            api_client: object | None = None,
            model: str | None = None,
        ) -> bool:
            checkpoint_calls.append(
                {
                    "task_id": task_id,
                    "snapshot": snapshot,
                    "api_client": api_client,
                    "model": model,
                }
            )
            return True

        def on_edit(self, _task_id: str, _file_path: str) -> None:
            return None

        def on_submission(self, _task_id: str) -> None:
            return None

    query_context = SimpleNamespace(
        tool_metadata=ExecutionMetadata(session_config="cfg", sandbox_id="sbx-1"),
        run_id="",
        tool_call_limit=10,
        tool_calls_used=0,
        session_state=None,
        api_messages_snapshot=[],
        api_client=object(),
        on_turn=None,
        exit_reason=QueryExitReason.TOOL_STOP,
        terminal_tools=set(),
    )

    async def _fake_run(_prompt: str):
        agent.display_messages = [
            ConversationMessage(role="assistant", content=[TextBlock(text="Working through the task")])
        ]
        query_context.on_turn(list(agent.display_messages))
        if False:
            yield None

    agent = SimpleNamespace(
        query_context=query_context,
        display_messages=[],
        total_usage=SimpleNamespace(input_tokens=0, output_tokens=0),
        model="test-model",
        run=_fake_run,
    )
    fake_team_run = SimpleNamespace(
        task_center=_TaskCenter(),
    )

    monkeypatch.setattr("team.runtime.runner.AgentRunTracker", SimpleNamespace(create=lambda **_: _Tracker()))
    monkeypatch.setattr("team.runtime.runner.spawn_agent", lambda *_args, **_kwargs: agent)
    monkeypatch.setattr("team.runtime.telemetry.estimate_final_context", lambda _messages: 0)
    monkeypatch.setattr("team.runtime.telemetry.persist_session_snapshot", lambda **_: None)
    monkeypatch.setattr("team.runtime.registry.get", lambda _team_run_id: fake_team_run)

    runner = _make_runner(
        session_config=SimpleNamespace(session_id="sess-1"),
        sandbox_id="sbx-1",
        printer=SimpleNamespace(
            raw_line=lambda who, body: raw_lines.append((who, body)),
            emit=lambda _event: None,
        ),
        team_metrics={"structured_log_path": str(structured_log_path)},
    )
    ctx = sweevo_team_runner.TeamAgentContext(
        user_message="Fix it",
        tool_metadata=ExecutionMetadata(team_run_id="TR1", work_item_id="W1"),
    )

    asyncio.run(
        runner(
            SimpleNamespace(name="developer", allowed_triggers=["tc_note"], model_copy=lambda update: SimpleNamespace(name="developer", allowed_triggers=["tc_note"], **update)),
            ctx,
        )
    )

    assert checkpoint_calls and checkpoint_calls[0]["task_id"] == "W1"
    assert checkpoint_calls[0]["model"] == "test-model"

    events = [json.loads(line) for line in structured_log_path.read_text(encoding="utf-8").splitlines()]
    hook_events = [event for event in events if event.get("event") == "external_hook"]
    assert [event["status"] for event in hook_events] == ["started", "completed"]
    assert hook_events
    assert all(event["hook"] == "tc_note" for event in hook_events)
    assert any("status=started" in body for _, body in raw_lines)
    assert any("status=completed" in body for _, body in raw_lines)


def test_make_runner_skips_tc_note_when_trigger_not_allowed(monkeypatch, tmp_path):
    """Checkpoint should NOT fire when allowed_triggers omits 'tc_note'."""
    checkpoint_calls: list[dict[str, object]] = []
    structured_log_path = tmp_path / "log.jsonl"

    class _Tracker:
        run_id = "R1"
        def finish(self, **_kw): pass

    class _TaskCenter:
        def __init__(self) -> None:
            self.activity = self

        def tick(self, _task_id: str) -> None:
            return None

        def should_take_note(self, _task_id: str) -> str | None:
            return "turn"  # always eligible

        async def check(self, task_id, *, snapshot=None, api_client=None, model=None):
            checkpoint_calls.append({"task_id": task_id})
            return True

        def on_edit(self, _task_id: str, _file_path: str) -> None:
            return None

        def on_submission(self, _task_id: str) -> None:
            return None

    query_context = SimpleNamespace(
        tool_metadata=ExecutionMetadata(session_config="cfg", sandbox_id="sbx-1"),
        run_id="",
        tool_call_limit=10,
        tool_calls_used=0,
        session_state=None,
        api_messages_snapshot=[],
        api_client=object(),
        on_turn=None,
        exit_reason=QueryExitReason.TOOL_STOP,
        terminal_tools=set(),
    )

    async def _fake_run(_prompt: str):
        agent.display_messages = [
            ConversationMessage(role="assistant", content=[TextBlock(text="Working")])
        ]
        query_context.on_turn(list(agent.display_messages))
        if False:
            yield None

    agent = SimpleNamespace(
        query_context=query_context,
        display_messages=[],
        total_usage=SimpleNamespace(input_tokens=0, output_tokens=0),
        model="test-model",
        run=_fake_run,
    )
    fake_team_run = SimpleNamespace(
        task_center=_TaskCenter(),
    )

    monkeypatch.setattr("team.runtime.runner.AgentRunTracker", SimpleNamespace(create=lambda **_: _Tracker()))
    monkeypatch.setattr("team.runtime.runner.spawn_agent", lambda *_args, **_kwargs: agent)
    monkeypatch.setattr("team.runtime.telemetry.estimate_final_context", lambda _messages: 0)
    monkeypatch.setattr("team.runtime.telemetry.persist_session_snapshot", lambda **_: None)
    monkeypatch.setattr("team.runtime.registry.get", lambda _team_run_id: fake_team_run)

    runner = _make_runner(
        session_config=SimpleNamespace(session_id="sess-1"),
        sandbox_id="sbx-1",
        printer=SimpleNamespace(
            raw_line=lambda who, body: None,
            emit=lambda _event: None,
        ),
        team_metrics={"structured_log_path": str(structured_log_path)},
    )
    ctx = sweevo_team_runner.TeamAgentContext(
        user_message="Fix it",
        tool_metadata=ExecutionMetadata(team_run_id="TR1", work_item_id="W1"),
    )

    # Agent definition WITHOUT tc_note in allowed_triggers
    asyncio.run(
        runner(
            SimpleNamespace(name="scout", allowed_triggers=[], model_copy=lambda update: SimpleNamespace(name="scout", allowed_triggers=[], **update)),
            ctx,
        )
    )

    # Checkpoint should NOT have been called despite should_take_note returning "turn"
    assert checkpoint_calls == []


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
            status=TeamRunStatus.SUCCEEDED,
            sandbox_id="sbx-1",
            budget_state=SimpleNamespace(replans_used=2),
            task_center=SimpleNamespace(
                graph={
                    "A": Task(
                        id="A",
                        team_run_id="TR1",
                        agent_name="developer",
                        status=TaskStatus.DONE,
                        objective="task",
                    ),
                    "B": Task(
                        id="B",
                        team_run_id="TR1",
                        agent_name="validator",
                        status=TaskStatus.DONE,
                        objective="task",
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

    assert result["replans_used"] == 2
    assert result["checkpoints"][-1]["label"] == "durable:complete:developer:A"
    assert result["latest_checkpoint_id"] == "cp-2"
    assert result["latest_checkpoint_label"] == "durable:complete:developer:A"
    assert any(
        body == "[team_stats] tasks=2 max_depth=1 agent_runs=4 checkpoints=2 replans=2"
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
        objective="plan",
        depth=0,
    )
    child = Task(
        id="child-1",
        team_run_id="TR1",
        agent_name="developer",
        status=TaskStatus.READY,
        objective="child task",
        deps=["root-1"],
        depth=1,
    )
    team_run = SimpleNamespace(task_center=SimpleNamespace(graph={root.id: root, child.id: child}))

    _emit_dispatcher_dag(printer, team_run, trigger_agent="team_planner")

    assert lines[0] == ("team", "[dag] after=team_planner nodes=2")
    assert any("root-1 agent=team_planner" in body for _, body in lines[1:])
    assert any("child-1 agent=developer" in body and "deps=['root-1']" in body for _, body in lines[1:])
