"""Registration and schema checks for Phase 03 submission tools."""

from __future__ import annotations

import pytest
from pydantic import ValidationError

from agents import AgentDefinition
from agents import AgentKind
from tools._framework.factory import ToolFactoryContext, create_tool, has_tool
from tools.submission.planner import PlanTaskInput


PHASE03_TOOLS = (
    "submit_full_plan",
    "submit_partial_plan",
    "submit_execution_handoff",
    "submit_execution_success",
    "submit_execution_failure",
    "submit_verification_success",
    "submit_verification_failure",
    "submit_evaluation_success",
    "submit_evaluation_failure",
    "ask_advisor",
    "submit_advisor_feedback",
    "ask_resolver",
    "submit_resolver_result",
    "submit_exploration_result",
)


def test_submission_tools_registered() -> None:
    assert all(has_tool(name) for name in PHASE03_TOOLS)


def test_submission_tools_are_terminal_except_helper_requests() -> None:
    non_terminal = {"ask_advisor", "ask_resolver"}
    ctx = ToolFactoryContext()

    for name in PHASE03_TOOLS:
        tool = create_tool(name, ctx)
        assert tool.is_terminal_tool is (name not in non_terminal)


def test_custom_generator_agent_can_declare_mission_solution_terminal() -> None:
    AgentDefinition(
        name="custom_generator",
        description="Custom generator agent.",
        agent_kind=AgentKind.EXECUTOR,
        terminals=["submit_execution_handoff"],
    )
    assert has_tool("submit_execution_handoff")


def test_plan_rendered_prompt_rejects_extra_keys() -> None:
    with pytest.raises(ValidationError):
        PlanTaskInput.model_validate(
            {"id": "a", "agent_name": "executor", "deps": [], "extra": "nope"}
        )
