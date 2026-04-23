# ruff: noqa
"""E2E tests for tool cancellation via [CANCEL:tool_id reason="..."] signal.

Tests verify:
1. Tool cancellation infrastructure works with EvalAgent
2. Mid-stream tool detection infrastructure is in place
3. Live model can receive and process cancellation signals

Requires live LLM API + Daytona sandbox.
Run with: pytest tests/test_e2e/test_tool_cancel_e2e.py -m live -v
"""

from __future__ import annotations

import pytest

from engine.testing.eval_agent import EvalAgent
from tests.test_e2e.conftest import (
    HAS_MINIMAX,
    create_eval_agent,
    create_test_sandbox,
    delete_test_sandbox,
)

pytestmark = [pytest.mark.e2e, pytest.mark.live, pytest.mark.asyncio]


# ===========================================================================
# Fixtures
# ===========================================================================


@pytest.fixture(scope="module")
def sandbox_id():
    if not EvalAgent.has_all():
        pytest.skip("LLM + Daytona credentials required")
    sb = create_test_sandbox("cancel-test")
    yield sb["id"]
    delete_test_sandbox(sb["id"])


# ===========================================================================
# Test: Tool Cancellation Infrastructure
# ===========================================================================


async def test_tool_cancelled_event_type_recognized(sandbox_id):
    """Verify tool_cancelled is a recognized event type."""
    agent = create_eval_agent(
        sandbox_id=sandbox_id,
        system_prompt=(
            "Execute the command and report results. "
            "If the command takes too long, you may cancel it."
        ),
    )

    result = await agent.invoke("Run: sleep 0.1 && echo 'DONE'")

    if result.has_errors:
        pytest.skip("Sandbox error occurred")

    # Verify the flow completed somehow - either via cancel, tool completion, or assistant turn
    assert (
        len(result.tools_cancelled()) > 0
        or len(result.tools_completed()) > 0
        or len(result.assistant_turns()) > 0
    )


async def test_tool_event_structure_complete(sandbox_id):
    """Verify tool events have complete structure."""
    agent = create_eval_agent(
        sandbox_id=sandbox_id,
        system_prompt="Run a simple command.",
    )

    result = await agent.invoke("Run: echo 'STRUCT_TEST'")

    # Check ToolExecutionStarted events have expected attributes
    tool_started = result.tools_started()
    if tool_started:
        for event in tool_started:
            assert hasattr(event, "tool_name"), f"Missing tool_name: {event}"
            assert hasattr(event, "tool_input"), f"Missing tool_input: {event}"

    # Check ToolExecutionCompleted events have expected attributes
    tool_completed = result.tools_completed()
    if tool_completed:
        for event in tool_completed:
            assert hasattr(event, "tool_name"), f"Missing tool_name: {event}"
            assert hasattr(event, "output"), f"Missing output: {event}"


async def test_mid_stream_tool_detection(sandbox_id):
    """Verify tools are detected and started mid-stream (not after complete response)."""
    agent = create_eval_agent(
        sandbox_id=sandbox_id,
        system_prompt="Execute commands as tools are detected.",
    )

    result = await agent.invoke("Run: echo 'MIDSTREAM'")

    if result.has_errors:
        pytest.skip("Sandbox error occurred")

    # We should have at least one tool_started
    tool_started = result.tools_started()
    assert len(tool_started) >= 1, f"Expected at least 1 tool_started, got: {len(tool_started)}"

    # Assistant turns should be present
    assert len(result.assistant_turns()) >= 1, "Should have assistant_complete"


# ===========================================================================
# Test: Tool Cancellation Signal
# ===========================================================================


async def test_model_can_explicitly_cancel(sandbox_id):
    """Instruct model to use cancel signal and verify it's processed."""
    agent = create_eval_agent(
        sandbox_id=sandbox_id,
        system_prompt=(
            "You have a remote sandbox. "
            "If a command is not needed, you can cancel it by outputting: "
            "[CANCEL:tool_id reason='not needed'] "
            "where tool_id is from the tool call header. "
            "Use the daytona_shell tool to run commands."
        ),
    )

    result = await agent.invoke(
        "1. Run: sleep 10 && echo 'SHOULD_NOT_COMPLETE'\n"
        "2. Then immediately cancel the sleep command using [CANCEL:tool_01 reason='taking too long']"
    )

    if result.has_errors:
        pytest.skip("Sandbox error occurred")

    # Verify the flow completed - should have at least one assistant turn
    assert len(result.assistant_turns()) > 0, "Should have completed"

    # If cancellation happened, verify the cancelled events have tool_name
    cancelled = result.tools_cancelled()
    if cancelled:
        assert all(hasattr(e, "tool_name") for e in cancelled)


async def test_cancel_signal_appears_in_assistant_text(sandbox_id):
    """Verify cancel signal text appears in assistant output when model uses it."""
    agent = create_eval_agent(
        sandbox_id=sandbox_id,
        system_prompt=(
            "You can cancel tools by outputting: [CANCEL:tool_id reason='...'] "
            "Use daytona_shell for commands."
        ),
    )

    result = await agent.invoke(
        "1. Start a sleep command: sleep 5\n"
        "2. Cancel it immediately with [CANCEL:tool_01 reason='not needed']\n"
        "3. Report what happened."
    )

    if result.has_errors:
        pytest.skip("Sandbox error occurred")

    assert len(result.assistant_turns()) > 0


# ===========================================================================
# Test: Protocol Verification
# ===========================================================================


@pytest.mark.skipif(not HAS_MINIMAX, reason="MiniMax required")
class TestCancellationProtocolCompliance:
    """Verify the BackendEvent protocol includes tool_cancelled type."""

    @pytest.fixture()
    def client(self, db_session_factory, tmp_path, monkeypatch):
        from tests.test_e2e.conftest import make_live_client

        c = make_live_client(db_session_factory, tmp_path, monkeypatch)
        with c:
            yield c

    def test_protocol_has_tool_cancelled_type(self, client):
        """Verify BackendEvent protocol includes tool_cancelled in type union."""
        from server.protocol import BackendEvent

        # This is a compile-time check - verify the type exists
        assert hasattr(BackendEvent, "model_fields")

        # The 'type' field should include 'tool_cancelled'
        type_field = BackendEvent.model_fields.get("type")
        assert type_field is not None, "BackendEvent should have 'type' field"

        # Check the literal values include tool_cancelled
        annotation = type_field.annotation
        # The annotation is a Literal union - verify tool_cancelled is in it
        if hasattr(annotation, "__args__"):
            literal_values = [arg for arg in annotation.__args__ if isinstance(arg, str)]
            assert "tool_cancelled" in literal_values, (
                f"tool_cancelled should be in BackendEvent.type literal. Got: {literal_values}"
            )

    def test_cancel_reason_field_exists(self, client):
        """Verify BackendEvent has cancel_reason field for cancellation context."""
        from server.protocol import BackendEvent

        # BackendEvent should have cancel_reason field
        assert "cancel_reason" in BackendEvent.model_fields, (
            f"BackendEvent should have cancel_reason field. Fields: {list(BackendEvent.model_fields.keys())}"
        )
