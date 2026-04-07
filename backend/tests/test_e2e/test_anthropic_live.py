# ruff: noqa
"""Live end-to-end tests for the Anthropic native client.

Uses EvalAgent for credential loading from ~/.ephemeralos/settings.json.
Run with: .venv/bin/python -m pytest backend/tests/test_e2e/test_anthropic_live.py -v
"""

from __future__ import annotations

import pytest

from engine.testing.eval_agent import EvalAgent
from tests.test_e2e.conftest import create_eval_agent
from message import ConversationMessage
from providers.clients.anthropic_native import AnthropicClient
from providers.types import (
    ApiMessageRequest,
    ApiTextDeltaEvent,
    ApiThinkingDeltaEvent,
    ApiToolUseDeltaEvent,
    ApiMessageCompleteEvent,
)

pytestmark = [pytest.mark.e2e, pytest.mark.live]

HAS_CREDENTIALS = EvalAgent.has_credentials()


@pytest.fixture(scope="module")
def agent():
    """Create an EvalAgent for credential access."""
    if not HAS_CREDENTIALS:
        pytest.skip("No LLM credentials configured")
    return create_eval_agent()


@pytest.fixture(scope="module")
def model(agent):
    """Get the model name from the agent's settings."""
    return agent.model


@pytest.fixture
def client(agent):
    """Access the raw API client for streaming protocol tests."""
    raw = agent.api_client
    if not isinstance(raw, AnthropicClient):
        pytest.skip("Raw client is not AnthropicClient (api_format may not be 'anthropic')")
    return raw


def _user_message(text: str) -> ConversationMessage:
    """Build a minimal user ConversationMessage."""
    return ConversationMessage.from_user_text(text)


# ---------------------------------------------------------------------------
# 1. Simple text response
# ---------------------------------------------------------------------------


@pytest.mark.skipif(not HAS_CREDENTIALS, reason="No credentials")
@pytest.mark.asyncio
async def test_simple_text_response(client, model):
    """Stream a simple reply and verify text delta + complete events."""
    request = ApiMessageRequest(
        model=model,
        messages=[_user_message("Say hello in exactly 3 words")],
        max_tokens=64,
    )

    events = []
    async for event in client.stream_message(request):
        events.append(event)

    text_events = [e for e in events if isinstance(e, ApiTextDeltaEvent)]
    thinking_events = [e for e in events if isinstance(e, ApiThinkingDeltaEvent)]
    complete_events = [e for e in events if isinstance(e, ApiMessageCompleteEvent)]

    # Some models stream text deltas, others stream thinking deltas
    assert len(text_events) >= 1 or len(thinking_events) >= 1, (
        "Expected at least one ApiTextDeltaEvent or ApiThinkingDeltaEvent"
    )
    assert len(complete_events) == 1, "Expected exactly one ApiMessageCompleteEvent"
    assert complete_events[-1] is events[-1], "ApiMessageCompleteEvent must be the last event"


# ---------------------------------------------------------------------------
# 2. Tool use mid-stream ordering
# ---------------------------------------------------------------------------


@pytest.mark.skipif(not HAS_CREDENTIALS, reason="No credentials")
@pytest.mark.asyncio
async def test_tool_use_mid_stream_ordering(client, model):
    """Validate that ApiToolUseDeltaEvent arrives BEFORE ApiMessageCompleteEvent."""
    weather_tool = {
        "name": "get_weather",
        "description": "Get weather for a city",
        "input_schema": {
            "type": "object",
            "properties": {"city": {"type": "string"}},
            "required": ["city"],
        },
    }

    request = ApiMessageRequest(
        model=model,
        messages=[_user_message("What's the weather in Tokyo? Use the get_weather tool.")],
        tools=[weather_tool],
        max_tokens=256,
    )

    events = []
    async for event in client.stream_message(request):
        events.append(event)

    tool_events = [e for e in events if isinstance(e, ApiToolUseDeltaEvent)]
    complete_events = [e for e in events if isinstance(e, ApiMessageCompleteEvent)]

    assert len(tool_events) >= 1, "Expected at least one ApiToolUseDeltaEvent"
    assert len(complete_events) == 1, "Expected exactly one ApiMessageCompleteEvent"

    first_tool_idx = next(i for i, e in enumerate(events) if isinstance(e, ApiToolUseDeltaEvent))
    complete_idx = next(i for i, e in enumerate(events) if isinstance(e, ApiMessageCompleteEvent))
    assert first_tool_idx < complete_idx, (
        f"Tool event at index {first_tool_idx} must precede complete event at index {complete_idx}"
    )

    tool_event = tool_events[0]
    assert tool_event.name == "get_weather", (
        f"Expected tool name 'get_weather', got '{tool_event.name}'"
    )
    assert "city" in tool_event.input, f"Expected 'city' in tool input, got {tool_event.input}"


# ---------------------------------------------------------------------------
# 3. Multiple tools arrive in order
# ---------------------------------------------------------------------------


@pytest.mark.skipif(not HAS_CREDENTIALS, reason="No credentials")
@pytest.mark.asyncio
async def test_multiple_tools_arrive_in_order(client, model):
    """Two tool calls should arrive sequentially, both before ApiMessageCompleteEvent."""
    weather_tool = {
        "name": "get_weather",
        "description": "Get weather for a city",
        "input_schema": {
            "type": "object",
            "properties": {"city": {"type": "string"}},
            "required": ["city"],
        },
    }
    time_tool = {
        "name": "get_time",
        "description": "Get the current time in a city",
        "input_schema": {
            "type": "object",
            "properties": {"city": {"type": "string"}},
            "required": ["city"],
        },
    }

    request = ApiMessageRequest(
        model=model,
        messages=[
            _user_message(
                "Get the weather in Tokyo and the current time in London. Use both tools."
            )
        ],
        tools=[weather_tool, time_tool],
        max_tokens=512,
    )

    events = []
    async for event in client.stream_message(request):
        events.append(event)

    tool_events = [e for e in events if isinstance(e, ApiToolUseDeltaEvent)]
    complete_events = [e for e in events if isinstance(e, ApiMessageCompleteEvent)]

    assert len(tool_events) == 2, f"Expected 2 ApiToolUseDeltaEvent, got {len(tool_events)}"
    assert len(complete_events) == 1, "Expected exactly one ApiMessageCompleteEvent"

    tool_indices = [i for i, e in enumerate(events) if isinstance(e, ApiToolUseDeltaEvent)]
    complete_idx = next(i for i, e in enumerate(events) if isinstance(e, ApiMessageCompleteEvent))

    for idx in tool_indices:
        assert idx < complete_idx, (
            f"Tool event at index {idx} must precede complete event at index {complete_idx}"
        )

    assert tool_indices[0] < tool_indices[1], (
        "First tool event must arrive before second tool event"
    )


# ---------------------------------------------------------------------------
# 4. Usage reported
# ---------------------------------------------------------------------------


@pytest.mark.skipif(not HAS_CREDENTIALS, reason="No credentials")
@pytest.mark.asyncio
async def test_usage_reported(client, model):
    """ApiMessageCompleteEvent must contain positive token usage counters."""
    request = ApiMessageRequest(
        model=model,
        messages=[_user_message("What is 2 + 2?")],
        max_tokens=64,
    )

    events = []
    async for event in client.stream_message(request):
        events.append(event)

    complete_events = [e for e in events if isinstance(e, ApiMessageCompleteEvent)]
    assert len(complete_events) == 1, "Expected exactly one ApiMessageCompleteEvent"

    usage = complete_events[0].usage
    assert usage.input_tokens > 0, f"input_tokens must be > 0, got {usage.input_tokens}"
    assert usage.output_tokens > 0, f"output_tokens must be > 0, got {usage.output_tokens}"


# ---------------------------------------------------------------------------
# 5. Provider routing
# ---------------------------------------------------------------------------


@pytest.mark.skipif(not HAS_CREDENTIALS, reason="No credentials")
def test_provider_routing(agent):
    """make_api_client returns AnthropicClient when api_format='anthropic'."""
    if agent.settings.api_format != "anthropic":
        pytest.skip("api_format is not 'anthropic'")

    assert isinstance(agent.api_client, AnthropicClient), (
        f"Expected AnthropicClient, got {type(agent.api_client).__name__}"
    )
