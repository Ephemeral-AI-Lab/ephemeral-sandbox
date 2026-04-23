from __future__ import annotations

import asyncio
import sys
from types import ModuleType, SimpleNamespace
from unittest.mock import AsyncMock

from benchmarks.sweevo.__main__ import _collect_health_issues
from benchmarks.sweevo.models import SWEEvoInstance
from benchmarks.sweevo.runner import run_sweevo_with_agent
from team.runtime.telemetry import (
    background_tool_names_from_messages as _background_tool_names_from_messages,
)
from message.messages import ConversationMessage, ToolUseBlock


def _instance() -> SWEEvoInstance:
    return SWEEvoInstance(
        instance_id="pydantic__pydantic_v2.6.0b1_v2.6.0",
        repo="pydantic/pydantic",
        base_commit="abc123",
        problem_statement="",
        patch="",
        fail_to_pass=["tests/test_discriminated_union.py::test_presence_of_discriminator"],
        pass_to_pass=["tests/test_json_schema.py::test_alias_same"],
        docker_image="xingyaoww/sweb.eval.x86_64.pydantic_s_pydantic-8583",
        test_cmds="pytest --continue-on-collection-errors -rA",
        environment_setup_commit="",
    )


def test_background_tool_names_from_messages_only_keeps_explicit_background_calls():
    messages = [
        ConversationMessage(
            role="assistant",
            content=[
                ToolUseBlock(name="daytona_shell", input={"code": "print(1)"}),
                ToolUseBlock(
                    name="daytona_shell",
                    input={"code": "print(2)", "background": True},
                ),
                ToolUseBlock(
                    name="run_subagent",
                    input={"agent_name": "scout", "background": True},
                ),
                ToolUseBlock(
                    name="daytona_shell",
                    input={"code": "print(3)", "background": False},
                ),
            ],
        )
    ]

    assert _background_tool_names_from_messages(messages) == [
        "daytona_shell",
        "run_subagent",
    ]


def test_run_sweevo_with_agent_returns_structured_grading(monkeypatch):
    import benchmarks.sweevo as sweevo_pkg
    from benchmarks.sweevo import runner as sweevo_runner

    instance = _instance()
    printer = SimpleNamespace(flush=lambda: None)
    captured: dict[str, object] = {}

    fake_team_runner = ModuleType("benchmarks.sweevo.team_runner")

    async def _fake_run_team(*args, **kwargs):
        captured["team_name"] = kwargs.get("team_name")
        return {
            "status": "succeeded",
            "team_name": kwargs.get("team_name"),
            "work_items": 2,
            "team_run_id": "TR-test",
            "sandbox_id": "sbx-1",
            "session_id": "sess-test",
            "structured_log_path": None,
            "usage": None,
            "usage_by_model": [],
            "checkpoints": [],
            "checkpoint_ids": [],
            "latest_checkpoint_id": None,
            "latest_checkpoint_label": None,
            "max_depth_reached": 0,
            "agent_runs": 0,
            "agent_counts": {},
            "replans_used": 0,
            "budgets": {"max_tasks": 40, "max_depth": 5, "max_plan_size": 12},
            "resumed_from": None,
            "resumed_from_checkpoint": None,
        }

    fake_team_runner.run_sweevo_team = _fake_run_team
    monkeypatch.setitem(sys.modules, "benchmarks.sweevo.team_runner", fake_team_runner)
    monkeypatch.setattr(sweevo_pkg, "team_runner", fake_team_runner, raising=False)

    fake_sandbox_pkg = ModuleType("sandbox")
    fake_lifecycle = ModuleType("sandbox.lifecycle")
    fake_lifecycle.shutdown_cached_client_async = AsyncMock()
    monkeypatch.setitem(sys.modules, "sandbox", fake_sandbox_pkg)
    monkeypatch.setitem(sys.modules, "sandbox.lifecycle", fake_lifecycle)

    monkeypatch.setattr(sweevo_runner, "select_sweevo_instance", lambda **_: instance)
    monkeypatch.setattr(
        sweevo_runner,
        "create_sweevo_test_sandbox",
        AsyncMock(
            return_value={
                "sandbox_id": "sbx-1",
                "sandbox": {"id": "sbx-1"},
                "snapshot_name": "snap-1",
            }
        ),
    )
    monkeypatch.setattr(sweevo_runner, "_extract_combined_patch", AsyncMock(return_value="diff"))
    async def _fake_evaluate(instance_arg, result, sandbox_id, repo_dir="/testbed"):
        assert instance_arg is instance
        assert sandbox_id == "sbx-1"
        assert repo_dir == "/testbed"
        result.resolved = False
        result.fix_rate = 0.0
        result.fail_to_pass_passed = 0
        result.fail_to_pass_total = 1
        result.pass_to_pass_broken = 1
        result.pass_to_pass_total = 1
        return result

    monkeypatch.setattr(sweevo_runner, "evaluate_sweevo_result", _fake_evaluate)

    result = asyncio.run(
        run_sweevo_with_agent(
            printer=printer,
            instance_id=instance.instance_id,
            team_name="sweevo-team-glm5.1",
            register_snapshot=False,
        )
    )

    assert captured["team_name"] == "sweevo-team-glm5.1"
    assert result["team_name"] == "sweevo-team-glm5.1"
    assert result["grading"] == {
        "resolved": False,
        "fix_rate": 0.0,
        "fail_to_pass_passed": 0,
        "fail_to_pass_total": 1,
        "pass_to_pass_broken": 1,
        "pass_to_pass_total": 1,
        "status": "completed",
    }


def test_collect_health_issues_includes_unresolved_grading():
    issues = _collect_health_issues(
        {
            "team_status": "succeeded",
            "grading": {
                "resolved": False,
                "fail_to_pass_passed": 0,
                "fail_to_pass_total": 1,
                "pass_to_pass_broken": 1,
                "pass_to_pass_total": 5,
                "fix_rate": 0.0,
            },
        }
    )

    assert issues == ["f2p=0/1", "p2p_broken=1/5"]


def test_run_sweevo_with_agent_resumes_existing_team_run(monkeypatch):
    import benchmarks.sweevo as sweevo_pkg
    from benchmarks.sweevo import runner as sweevo_runner

    instance = _instance()
    printer = SimpleNamespace(flush=lambda: None)

    fake_team_runner = ModuleType("benchmarks.sweevo.team_runner")

    async def _fake_resume_team(*args, **kwargs):
        return {
            "status": "succeeded",
            "work_items": 3,
            "team_run_id": "TR-1",
            "sandbox_id": "sbx-resume",
            "session_id": "sess-resume",
            "structured_log_path": None,
            "usage": None,
            "usage_by_model": [],
            "checkpoints": [{"id": "cp-1", "label": "initial", "sequence": 0}],
            "checkpoint_ids": ["cp-1"],
            "latest_checkpoint_id": "cp-1",
            "latest_checkpoint_label": "initial",
            "max_depth_reached": 1,
            "agent_runs": 2,
            "agent_counts": {"developer": 1, "validator": 1},
            "replans_used": 0,
            "budgets": {"max_tasks": 40, "max_depth": 5, "max_plan_size": 12},
            "resumed_from": "TR-1",
            "resumed_from_checkpoint": None,
        }

    async def _unexpected_run_team(*args, **kwargs):
        raise AssertionError("fresh team run should not be used for resume")

    fake_team_runner.resume_sweevo_team = _fake_resume_team
    fake_team_runner.run_sweevo_team = _unexpected_run_team
    monkeypatch.setitem(sys.modules, "benchmarks.sweevo.team_runner", fake_team_runner)
    monkeypatch.setattr(sweevo_pkg, "team_runner", fake_team_runner, raising=False)

    fake_sandbox_pkg = ModuleType("sandbox")
    fake_lifecycle = ModuleType("sandbox.lifecycle")
    fake_lifecycle.shutdown_cached_client_async = AsyncMock()
    monkeypatch.setitem(sys.modules, "sandbox", fake_sandbox_pkg)
    monkeypatch.setitem(sys.modules, "sandbox.lifecycle", fake_lifecycle)

    monkeypatch.setattr(sweevo_runner, "select_sweevo_instance", lambda **_: instance)
    monkeypatch.setattr(
        sweevo_runner,
        "create_sweevo_test_sandbox",
        AsyncMock(side_effect=AssertionError("resume path should not create a sandbox")),
    )
    monkeypatch.setattr(sweevo_runner, "_extract_combined_patch", AsyncMock(return_value="diff"))
    async def _fake_evaluate(instance_arg, result, sandbox_id, repo_dir="/testbed"):
        assert instance_arg is instance
        assert sandbox_id == "sbx-resume"
        assert repo_dir == "/testbed"
        result.resolved = True
        result.fix_rate = 1.0
        result.fail_to_pass_passed = 1
        result.fail_to_pass_total = 1
        result.pass_to_pass_broken = 0
        result.pass_to_pass_total = 1
        return result

    monkeypatch.setattr(sweevo_runner, "evaluate_sweevo_result", _fake_evaluate)

    result = asyncio.run(
        run_sweevo_with_agent(
            printer=printer,
            instance_id=instance.instance_id,
            register_snapshot=False,
            resume_team_run_id="TR-1",
        )
    )

    assert result["team_run_id"] == "TR-1"
    assert result["sandbox"]["id"] == "sbx-resume"
    assert result["grading"]["resolved"] is True
