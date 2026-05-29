"""Offline integration test for the task_center_runner wiring.

Verifies that the framework's public surface is importable, the squad runner
constructs without an SWE-EVO instance, the agent registry can be installed +
restored cleanly, and a scenario can produce a planner response from a fresh
ScenarioContext — all without invoking Daytona or Postgres.
"""

from __future__ import annotations

import inspect
from types import SimpleNamespace

import pytest

from agents import AgentDefinition, AgentKind
from agents import list_definitions
from task_center_runner import RunReport, run_scenario
from task_center_runner.audit.bus import AuditEventBus
from task_center_runner.hooks.registry import HookSet, MutableMockState
from task_center_runner.scenarios.base import ScenarioContext
from task_center_runner.scenarios.correctness_testing import CorrectnessTesting
from task_center_runner.scenarios.full_case_user_input import FullCaseUserInput
from task_center_runner.scenarios.full_stack_adversarial import FullStackAdversarial
from task_center_runner.agent.mock.definitions import (
    mock_agent_definitions,
    registered_mock_agents,
)
from task_center_runner.agent.mock.runner import MockSquadRunner
from tools._framework.core.runtime import ExecutionMetadata
from tools.submission.planner import (
    submit_plan_closes_goal,
    submit_plan_defers_goal,
)


def test_runner_top_level_exports_are_callable() -> None:
    assert callable(run_scenario)
    assert RunReport.__module__ == "task_center_runner.core.runner"
    sig = inspect.signature(run_scenario)
    # de-sweevo-fied signature: no ``instance``, ``repo_dir`` is required, and
    # ``entry_prompt`` is required.
    params = sig.parameters
    assert "instance" not in params
    assert params["repo_dir"].default is inspect.Parameter.empty
    assert params["entry_prompt"].default is inspect.Parameter.empty
    assert params["sandbox_id"].default is inspect.Parameter.empty


def test_squad_runner_constructs_without_instance() -> None:
    bus = AuditEventBus()
    state = MutableMockState()
    runner = MockSquadRunner(
        repo_dir="/tmp/live_e2e_test_repo",
        bus=bus,
        scenario=CorrectnessTesting(),
        mutable_state=state,
    )
    assert runner._repo_dir == "/tmp/live_e2e_test_repo"  # noqa: SLF001
    assert not hasattr(runner, "_instance"), (
        "MockSquadRunner must not retain an SWE-EVO ``instance`` attribute "
        "after the de-sweevo migration"
    )
    # Probe paths are preserved verbatim per the migration spec.
    assert runner._probe_path() == ".ephemeralos/sweevo-mock/probe.txt"  # noqa: SLF001


def test_prompt_inspector_accepts_current_failed_attempt_heading(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    runner = MockSquadRunner(
        repo_dir="/tmp/live_e2e_test_repo",
        bus=AuditEventBus(),
        scenario=CorrectnessTesting(),
        mutable_state=MutableMockState(),
    )
    monkeypatch.setattr(
        runner,
        "_current_attempt_and_iteration",
        lambda _metadata: (
            SimpleNamespace(attempt_sequence_no=2),
            SimpleNamespace(sequence_no=1),
        ),
    )

    inspection = runner._inspect_prompt(  # noqa: SLF001
        prompt="\n".join(
            [
                "<goal>Do the retry work.</goal>",
                "<iteration iteration_no=\"1\" position=\"current\">",
                "<attempt attempt_no=\"1\">Attempt 1 failed.</attempt>",
                "</iteration>",
            ]
        ),
        agent_def=AgentDefinition(
            name="planner",
            description="test planner",
            agent_kind=AgentKind.PLANNER,
            terminals=["submit_plan_closes_goal", "submit_plan_defers_goal"],
            tool_call_limit=10,
        ),
        metadata=ExecutionMetadata(task_center_task_id="attempt-2:planner"),
    )

    assert inspection.checks["failed_attempts"]
    assert inspection.passed


def test_prompt_inspector_accepts_current_previous_iteration_sections(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    runner = MockSquadRunner(
        repo_dir="/tmp/live_e2e_test_repo",
        bus=AuditEventBus(),
        scenario=CorrectnessTesting(),
        mutable_state=MutableMockState(),
    )
    monkeypatch.setattr(
        runner,
        "_current_attempt_and_iteration",
        lambda _metadata: (
            SimpleNamespace(attempt_sequence_no=1),
            SimpleNamespace(sequence_no=2),
        ),
    )

    inspection = runner._inspect_prompt(  # noqa: SLF001
        prompt="\n".join(
            [
                "<goal>Continue the delegated workflow.</goal>",
                "<iteration iteration_no=\"1\" position=\"prior\">",
                "<task id=\"schema\" status=\"success\">Earlier result.</task>",
                "</iteration>",
                "<iteration iteration_no=\"2\" position=\"current\">",
                "Next slice.",
                "</iteration>",
            ]
        ),
        agent_def=AgentDefinition(
            name="planner",
            description="test planner",
            agent_kind=AgentKind.PLANNER,
            terminals=["submit_plan_closes_goal", "submit_plan_defers_goal"],
            tool_call_limit=10,
        ),
        metadata=ExecutionMetadata(task_center_task_id="attempt-1:planner"),
    )

    assert inspection.checks["previous_iteration_results"]
    assert inspection.passed


def test_prompt_inspector_accepts_planner_without_defer_terminal() -> None:
    runner = MockSquadRunner(
        repo_dir="/tmp/live_e2e_test_repo",
        bus=AuditEventBus(),
        scenario=CorrectnessTesting(),
        mutable_state=MutableMockState(),
    )

    inspection = runner._inspect_prompt(  # noqa: SLF001
        prompt="\n".join(
            [
                "<context>",
                "<goal>Close this delegated recursive goal.</goal>",
                "<iteration iteration_no=\"1\" position=\"current\">",
                "<iteration_goal>Close this delegated recursive goal.</iteration_goal>",
                "</iteration>",
                "</context>",
                "<Task Guidance>",
                "Use submit_plan_closes_goal to close this goal in one attempt.",
                "</Task Guidance>",
            ]
        ),
        agent_def=AgentDefinition(
            name="planner",
            description="test full-only planner",
            agent_kind=AgentKind.PLANNER,
            terminals=["submit_plan_closes_goal"],
            tool_call_limit=10,
        ),
        metadata=ExecutionMetadata(
            task_center_task_id="recursive-1:planner",
            extras={"active_terminals": ["submit_plan_closes_goal"]},
        ),
    )

    assert inspection.checks == {
        "goal": True,
        "current_iteration": True,
        "closes_goal_terminal": True,
        "no_defer_terminal": True,
    }
    assert inspection.passed


def test_registered_mock_agents_install_and_restore() -> None:
    initial = {d.name for d in list_definitions()}
    with registered_mock_agents():
        installed = {d.name for d in list_definitions()}
        expected = {d.name for d in mock_agent_definitions()}
        assert installed == expected
    after = {d.name for d in list_definitions()}
    assert after == initial


def test_mock_agent_definitions_have_neutral_descriptions() -> None:
    """De-sweevo: descriptions and system prompts must not mention 'SWE-EVO'."""
    for definition in mock_agent_definitions():
        assert "SWE-EVO" not in (definition.description or "")
        assert "SWE-EVO" not in (definition.system_prompt or "")


@pytest.mark.parametrize(
    "scenario_cls",
    [CorrectnessTesting, FullCaseUserInput, FullStackAdversarial],
)
def test_scenarios_register_hookset_cleanly(scenario_cls: type) -> None:
    scenario = scenario_cls()
    hook_set = HookSet()
    for hook in scenario.hooks():
        hook_set.register(hook)
    assert scenario.name in {
        "correctness_testing",
        "full_case_user_input",
        "full_stack_adversarial",
    }
    assert hasattr(scenario, "expected_event_sequence")


def test_full_stack_recursive_planner_without_defer_closes_goal() -> None:
    scenario = FullStackAdversarial()
    ctx = ScenarioContext(
        attempt=SimpleNamespace(attempt_sequence_no=1, evaluation_criteria=()),
        iteration=SimpleNamespace(sequence_no=1, workflow_id="recursive-goal"),
        workflow=SimpleNamespace(requested_by_task_id="parent-task:executor"),
        prompt="Run delegated recursive matrix.",
        metadata=ExecutionMetadata(
            agent_name="planner",
            extras={"active_terminals": ["submit_plan_closes_goal"]},
        ),
        audit_recorder=None,
        mutable_state=None,
        task_id="recursive-goal:planner",
        agent_name="planner",
        context_message=None,
    )

    spec = scenario.planner_response(ctx)

    assert spec.tool.name == submit_plan_closes_goal.name
    assert spec.tool.name != submit_plan_defers_goal.name
    assert "deferred_goal_for_next_iteration" not in spec.args
    task_ids = {task["id"] for task in spec.args["tasks"]}
    assert {
        "recursive_oversized_a",
        "recursive_oversized_b",
        "recursive_closure_report",
        "recursive_close_guard",
    } <= task_ids


def test_sweevo_image_environment_keeps_dataset_entrypoint_separate() -> None:
    """SWE-EVO image prompt wiring lives outside the generic runner."""
    from task_center_runner.environments.sweevo_image.fixtures import (
        run_scenario_on_sweevo_image,
    )

    assert callable(run_scenario_on_sweevo_image)
    assert inspect.signature(run_scenario_on_sweevo_image).parameters["instance"]
