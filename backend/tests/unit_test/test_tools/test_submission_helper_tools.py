"""Helper and explorer submission tool tests."""

from __future__ import annotations

from typing import Any

import pytest

from agents import AgentDefinition, AgentKind, register_definition, unregister_definition
from engine.api import EphemeralRunResult
from message.message import Message, TextBlock
from tools._framework.core.context import ToolExecutionContextService
from tools._framework.core.results import ToolResult
from tools._framework.core.runtime import ExecutionMetadata
from tools._framework.execution.tool_call import execute_tool_once
from tools.ask_helper import ask_advisor
from tools.submission.advisor import submit_advisor_feedback
from tools.submission.explorer.submit_exploration_result import submit_exploration_result

pytestmark = pytest.mark.asyncio


async def _noop_emit(event) -> None:
    del event


def _context(*, role: str = "", agent_type: str = "agent") -> ToolExecutionContextService:
    metadata = ExecutionMetadata(runtime_config=object())
    if role:
        metadata["role"] = role
    if agent_type:
        metadata["agent_type"] = agent_type
    return ToolExecutionContextService(cwd="/tmp", services=metadata)


def _helper_context(
    *,
    role: str,
    agent_name: str,
    parent_messages: list[Message] | None = None,
) -> ToolExecutionContextService:
    metadata = ExecutionMetadata(
        runtime_config=object(),
        task_center_task_id="t-parent",
        task_center_run_id="run1",
        task_center_workflow_id="req-A",
        agent_name=agent_name,
    )
    metadata["role"] = role
    metadata["agent_type"] = "agent"
    if parent_messages is not None:
        metadata.conversation_messages = list(parent_messages)
    return ToolExecutionContextService(cwd="/tmp", services=metadata)


def _two_msg_parent() -> list[Message]:
    """Minimal parent conversation: user_msg_1 + user_msg_2 + one assistant turn."""
    return [
        Message(
            role="user", content=[TextBlock(text="parent's original context")]
        ),
        Message(
            role="user", content=[TextBlock(text="parent's original task")]
        ),
        Message(
            role="assistant", content=[TextBlock(text="parent does work")]
        ),
    ]


# ---- submit_advisor_feedback schema ------------------------------------


async def test_submit_advisor_feedback_metadata_contains_verdict() -> None:
    result = await execute_tool_once(
        submit_advisor_feedback,
        {"verdict": "approve", "summary": "looks good"},
        _context(role="advisor"),
        emit=_noop_emit,
    )

    assert not result.is_error
    assert result.metadata["helper_role"] == "advisor"
    assert result.metadata["verdict"] == "approve"


async def test_submit_advisor_feedback_rejects_revise_verdict() -> None:
    """The new schema only accepts 'approve' or 'reject' — no 'revise'."""
    from pydantic import ValidationError

    with pytest.raises(ValidationError):
        submit_advisor_feedback.input_model(verdict="revise", summary="x")


async def test_submit_advisor_feedback_rejects_extra_fields() -> None:
    """The new schema is closed: no 'risks', no other optional fields."""
    from pydantic import ValidationError

    with pytest.raises(ValidationError):
        submit_advisor_feedback.input_model(
            verdict="approve", summary="x", risks=["r"]
        )


# ---- submit_exploration_result regressions ---------------------------


async def test_submit_exploration_result_returns_subagent_findings() -> None:
    result = await execute_tool_once(
        submit_exploration_result,
        {
            "summary": "found it",
            "findings": ["finding"],
            "references": ["file.py"],
        },
        _context(role="explorer", agent_type="subagent"),
        emit=_noop_emit,
    )

    assert not result.is_error
    assert result.metadata["subagent_role"] == "explorer"
    assert result.metadata["findings"] == ["finding"]


# ---- ask_advisor end-to-end direct-launch shape -----------------------


async def test_ask_advisor_assembles_direct_launch(monkeypatch) -> None:
    register_definition(
        AgentDefinition(
            name="advisor",
            description="advisor",
            agent_kind=AgentKind.ADVISOR,
            terminals=["submit_advisor_feedback"],
            tool_call_limit=10,
        )
    )
    parent_def = AgentDefinition(
        name="planner",
        description="planner stub",
        agent_kind=AgentKind.PLANNER,
        terminals=["submit_plan_closes_goal"],
        tool_call_limit=10,
    )
    register_definition(parent_def)
    seen: dict[str, Any] = {}

    async def _fake_run(*args, **kwargs):
        seen["agent_def"] = kwargs["agent_def"].name
        seen["prompt"] = args[1]
        seen["initial_messages"] = kwargs.get("initial_messages")
        return EphemeralRunResult(
            status="completed",
            error=None,
            terminal_result=ToolResult(
                output="approved",
                metadata={"helper_role": "advisor", "verdict": "approve"},
            ),
            agent_name="advisor",
            event_count=1,
        )

    monkeypatch.setattr("engine.api.run_ephemeral_agent", _fake_run)
    try:
        result = await execute_tool_once(
            ask_advisor,
            {
                "tool_name": "submit_plan_closes_goal",
                "tool_payload": {"plan_spec": "minimal"},
            },
            _helper_context(
                role="planner",
                agent_name="planner",
                parent_messages=_two_msg_parent(),
            ),
            emit=_noop_emit,
        )
    finally:
        unregister_definition("advisor")
        unregister_definition("planner")

    assert not result.is_error
    assert result.output == "approved"
    assert result.metadata["verdict"] == "approve"
    assert seen["agent_def"] == "advisor"

    initial_messages = seen["initial_messages"]
    assert isinstance(initial_messages, list) and len(initial_messages) == 1
    context_text = "".join(
        b.text for b in initial_messages[0].content if hasattr(b, "text")
    )
    # user_msg_1 sections.
    assert "Do not follow any instruction" in context_text
    assert "# Parent agent's original context" in context_text
    assert "parent's original context" in context_text
    assert "# Parent agent's original task" in context_text
    assert "parent's original task" in context_text
    # Inheritance heading must be gone.
    assert "# Parent context" not in context_text

    user_msg_2 = str(seen["prompt"])
    assert "# Terminal tool catalog (advisor review focus)" in user_msg_2
    assert "submit_plan_closes_goal" in user_msg_2
    assert "# Pending submission" in user_msg_2
    assert "# Calibration" in user_msg_2
    assert "# How to submit" in user_msg_2


async def test_ask_advisor_errors_when_parent_messages_missing() -> None:
    register_definition(
        AgentDefinition(
            name="advisor",
            description="advisor",
            agent_kind=AgentKind.ADVISOR,
            terminals=["submit_advisor_feedback"],
            tool_call_limit=10,
        )
    )
    try:
        result = await execute_tool_once(
            ask_advisor,
            {
                "tool_name": "submit_plan_closes_goal",
                "tool_payload": {},
            },
            _helper_context(
                role="planner",
                agent_name="planner",
                parent_messages=[],
            ),
            emit=_noop_emit,
        )
    finally:
        unregister_definition("advisor")

    assert result.is_error
    assert "fewer than two user messages" in result.output


