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
    _derive_planner_runtime_limits,
    _emit_dispatcher_dag,
    _make_context_builders,
    _make_runner,
)
from agents.types import AgentDefinition
from engine.core.query import QueryExitReason
from team.runtime.agent_context import TeamAgentContext
from message import ConversationMessage, TextBlock, ToolUseBlock
from team.definitions import (
    DEVELOPER,
    ROOT_PLANNER,
    SCOUT,
    TEAM_PLANNER,
    TEAM_REPLANNER,
    VALIDATOR,
)
from team.core.models import Task, TaskStatus, TeamDefinition, TeamRunStatus
from team.task_center.prompts import UserPromptContextParts
from tools.core.runtime import ExecutionMetadata


# ---------------------------------------------------------------------------
# Shared factories
# ---------------------------------------------------------------------------

def _spec(goal: str) -> dict[str, str]:
    return {
        "goal": goal,
        "detail": f"Detail for {goal}",
        "acceptance_criteria": f"Acceptance for {goal}",
    }


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
        task_center=SimpleNamespace(graph={}),
        wait=AsyncMock(),
    )
    for k, v in overrides.items():
        setattr(base, k, v)
    return base


def test_load_or_create_team_definition_uses_config_registry(monkeypatch):
    target = TeamDefinition(
        id="config-team",
        name="sweevo-team-glm5.1",
        description="config team",
        entry_planner=ROOT_PLANNER,
        roster={"planner": [ROOT_PLANNER]},
    )
    monkeypatch.setattr("team.definitions.get_team_definition", lambda _name: None)
    monkeypatch.setattr("team.definitions.get_team_definition", lambda name: target if name == target.name else None)

    session_factory = object()
    result = sweevo_team_runner._load_or_create_team_definition(
        session_factory,
        team_name="sweevo-team-glm5.1",
    )

    assert result is target


def test_load_or_create_team_definition_rejects_missing_config(monkeypatch):
    monkeypatch.setattr("team.definitions.get_team_definition", lambda _name: None)

    with pytest.raises(RuntimeError, match="backend/config/teams/sweevo_benchmark.md"):
        sweevo_team_runner._load_or_create_team_definition(
            object(),
            team_name="sweevo_benchmark",
        )


def test_load_or_create_team_definition_uses_current_builtin(monkeypatch):
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
    )
    monkeypatch.setattr("team.definitions.get_team_definition", lambda _name: builtin)

    session_factory = object()
    result = sweevo_team_runner._load_or_create_team_definition(
        session_factory,
        team_name="sweevo_benchmark",
    )

    assert result is builtin




@pytest.mark.asyncio
async def test_query_ctx_seeds_repo_root_for_daytona_and_ci():
    build_query_ctx = _make_context_builders("sbx-1", repo_dir="/testbed")
    template_context_for = AsyncMock(return_value=UserPromptContextParts(task_spec="Fix it"))
    ctx = await build_query_ctx(
        SimpleNamespace(
            name="developer",
            role="developer",
            terminal_tools=["submit_task_success", "request_replan"],
        ),
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
            spec=_spec("Fix it"),
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
        "hook": "helper_run",
        "team_run_id": "run-1",
        "work_item_id": "task-1",
        "status": "completed",
    })

    payload = json.loads(structured_log.read_text(encoding="utf-8").strip())
    assert payload["hook"] == "helper_run"
    assert lines == [
        (
            "team",
            "[external_hook] helper_run task=task-1 status=completed",
        )
    ]


def test_agent_overrides_attach_validator_skill_without_prompt_duplication():
    sweevo_team_runner._register_team_builtins()
    instance = _pydantic_instance()

    overrides = _build_agent_overrides(instance)

    assert "system_prompt" not in overrides[TEAM_PLANNER]
    assert "read_task_details" in overrides[TEAM_PLANNER]["tools"]
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
        SimpleNamespace(
            name="root_planner",
            role="planner",
            terminal_tools=["submit_plan"],
        ),
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
            spec=_spec("Root planning task"),
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

    def fake_team_run_store(base_dir):
        captured["base_dir"] = base_dir
        return sentinel

    monkeypatch.setattr(sweevo_team_runner, "TeamRunStore", fake_team_run_store)

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
    ctx = TeamAgentContext(
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
    monkeypatch.setattr("benchmarks.sweevo.telemetry.estimate_final_context", lambda _messages: 321)
    monkeypatch.setattr("benchmarks.sweevo.telemetry.persist_session_snapshot", lambda **_: None)

    runner = _make_runner(
        session_config=SimpleNamespace(session_id="sess-1"),
        sandbox_id="sbx-1",
        printer=SimpleNamespace(
            raw_line=lambda who, body: printed.append((who, body)),
            emit=lambda _event: None,
        ),
    )
    ctx = TeamAgentContext(
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


def test_make_runner_persists_work_result(monkeypatch):
    class _Tracker:
        run_id = "run-1"

        def finish(self, **_: object) -> None:
            return None

    final_text = (
        '{"tasks":[{"id":"dev-1","spec":{"goal":"Fix auth","detail":"Repair auth.",'
        '"acceptance_criteria":"Run auth tests."},"agent":"developer","deps":[],'
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
    monkeypatch.setattr("benchmarks.sweevo.telemetry.estimate_final_context", lambda _messages: 0)
    monkeypatch.setattr("benchmarks.sweevo.telemetry.persist_session_snapshot", lambda **_: None)
    monkeypatch.setattr("team.runtime.run_registry.get", lambda _team_run_id: None)

    runner = _make_runner(
        session_config=SimpleNamespace(session_id="sess-1"),
        sandbox_id="sbx-1",
        printer=None,
    )
    ctx = TeamAgentContext(
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
    monkeypatch.setattr("benchmarks.sweevo.telemetry.estimate_final_context", lambda _messages: 456)
    monkeypatch.setattr("benchmarks.sweevo.telemetry.persist_session_snapshot", lambda **_: None)
    monkeypatch.setattr("team.runtime.run_registry.get", lambda _team_run_id: None)

    runner = _make_runner(
        session_config=SimpleNamespace(session_id="sess-1"),
        sandbox_id="sbx-1",
        printer=None,
        team_metrics={"agent_run_log_dir": str(tmp_path)},
    )
    ctx = TeamAgentContext(
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
    monkeypatch.setattr("benchmarks.sweevo.telemetry.estimate_final_context", lambda _messages: 0)
    monkeypatch.setattr("benchmarks.sweevo.telemetry.persist_session_snapshot", lambda **_: None)
    monkeypatch.setattr("team.runtime.run_registry.get", lambda _team_run_id: None)

    runner = _make_runner(
        session_config=SimpleNamespace(session_id="sess-1"),
        sandbox_id="sbx-1",
        printer=None,
        team_metrics={"agent_run_log_dir": str(tmp_path)},
    )
    ctx = TeamAgentContext(
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


def test_finalize_team_result_surfaces_retry_replan_metadata(monkeypatch):
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
                        spec=_spec("task"),
                    ),
                    "B": Task(
                        id="B",
                        team_run_id="TR1",
                        agent_name="validator",
                        status=TaskStatus.DONE,
                        spec=_spec("task"),
                        depth=1,
                    ),
                },
            ),
        ),
        session_config=SimpleNamespace(session_id="sess-1"),
        team_metrics={
            "agent_runs": 4,
            "agent_counts": Counter({"developer": 2, "validator": 2}),
        },
        budgets=SimpleNamespace(
            max_tasks=10,
            max_depth=5,
            max_plan_size=6,
        ),
        printer=SimpleNamespace(raw_line=lambda who, body: printed.append((who, body))),
    )

    assert result["replans_used"] == 2
    assert any(
        body == "[team_stats] tasks=2 max_depth=1 agent_runs=4 replans=2"
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
        spec=_spec("plan"),
        depth=0,
    )
    child = Task(
        id="child-1",
        team_run_id="TR1",
        agent_name="developer",
        status=TaskStatus.READY,
        spec=_spec("child task"),
        deps=["root-1"],
        depth=1,
    )
    team_run = SimpleNamespace(task_center=SimpleNamespace(graph={root.id: root, child.id: child}))

    _emit_dispatcher_dag(printer, team_run, trigger_agent="team_planner")

    assert lines[0] == ("team", "[dag] after=team_planner nodes=2")
    assert any("root-1 agent=team_planner" in body for _, body in lines[1:])
    assert any("child-1 agent=developer" in body and "deps=['root-1']" in body for _, body in lines[1:])
