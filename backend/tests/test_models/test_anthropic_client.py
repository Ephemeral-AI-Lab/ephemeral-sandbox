"""Tests for the Anthropic-native streaming client."""

from __future__ import annotations

import json
from typing import Any
from unittest.mock import AsyncMock, MagicMock, patch

import anthropic
import pytest

from providers.clients.anthropic_native import AnthropicClient, MAX_RETRIES
from providers.types import (
    ApiMessageCompleteEvent,
    ApiMessageRequest,
    ApiTextDeltaEvent,
    ApiThinkingDeltaEvent,
    ApiToolUseDeltaEvent,
    UsageSnapshot,
)
from providers.errors import AuthenticationFailure, RateLimitFailure
from message import ConversationMessage, TextBlock


# ---------------------------------------------------------------------------
# Mock helpers
# ---------------------------------------------------------------------------


class MockDelta:
    def __init__(self, type: str, **kwargs: Any) -> None:
        self.type = type
        for k, v in kwargs.items():
            setattr(self, k, v)


class MockContentBlock:
    def __init__(self, type: str, **kwargs: Any) -> None:
        self.type = type
        for k, v in kwargs.items():
            setattr(self, k, v)


class MockEvent:
    def __init__(self, type: str, **kwargs: Any) -> None:
        self.type = type
        for k, v in kwargs.items():
            setattr(self, k, v)


class MockUsage:
    def __init__(self, input_tokens: int = 0, output_tokens: int = 0) -> None:
        self.input_tokens = input_tokens
        self.output_tokens = output_tokens


class MockFinalMessage:
    def __init__(
        self,
        content: list[Any] | None = None,
        usage: MockUsage | None = None,
        stop_reason: str = "end_turn",
    ) -> None:
        self.content = content or []
        self.usage = usage or MockUsage()
        self.stop_reason = stop_reason


class MockStream:
    """Async context-manager + async-iterator that replays canned events."""

    def __init__(self, events: list[MockEvent], final_message: MockFinalMessage) -> None:
        self._events = events
        self._final_message = final_message

    async def __aenter__(self) -> MockStream:
        return self

    async def __aexit__(self, *args: Any) -> None:
        pass

    async def __aiter__(self):
        for event in self._events:
            yield event

    async def get_final_message(self) -> MockFinalMessage:
        return self._final_message


def _make_text_block(text: str) -> MockContentBlock:
    return MockContentBlock(type="text", text=text)


def _make_tool_use_block(id: str, name: str, input: dict[str, Any]) -> MockContentBlock:
    return MockContentBlock(type="tool_use", id=id, name=name, input=input)


def _make_thinking_block(text: str) -> MockContentBlock:
    return MockContentBlock(type="thinking", thinking=text, text=text)


def _make_request(messages: list[ConversationMessage] | None = None) -> ApiMessageRequest:
    """Build a minimal ApiMessageRequest for testing."""
    msgs = messages or [ConversationMessage.from_user_text("hello")]
    return ApiMessageRequest(model="claude-sonnet-4-20250514", messages=msgs)


def _build_client() -> AnthropicClient:
    """Construct an AnthropicClient with a dummy API key."""
    return AnthropicClient(api_key="sk-test-key")


async def _collect_events(client: AnthropicClient, request: ApiMessageRequest) -> list[Any]:
    """Drain all events from stream_message into a list."""
    events: list[Any] = []
    async for ev in client.stream_message(request):
        events.append(ev)
    return events


# ---------------------------------------------------------------------------
# Tests
# ---------------------------------------------------------------------------


class TestTextOnlyResponse:
    @pytest.mark.asyncio
    async def test_text_only_response(self) -> None:
        """Text-only stream yields ApiTextDeltaEvent per delta then ApiMessageCompleteEvent."""
        events = [
            MockEvent(
                type="content_block_start",
                index=0,
                content_block=MockContentBlock(type="text"),
            ),
            MockEvent(
                type="content_block_delta",
                index=0,
                delta=MockDelta(type="text_delta", text="Hello"),
            ),
            MockEvent(
                type="content_block_delta",
                index=0,
                delta=MockDelta(type="text_delta", text=" world"),
            ),
            MockEvent(
                type="content_block_delta",
                index=0,
                delta=MockDelta(type="text_delta", text="!"),
            ),
            MockEvent(type="content_block_stop", index=0),
            MockEvent(type="message_stop"),
        ]
        final = MockFinalMessage(
            content=[_make_text_block("Hello world!")],
            usage=MockUsage(input_tokens=10, output_tokens=5),
        )

        client = _build_client()
        client._client.messages.stream = MagicMock(return_value=MockStream(events, final))

        result = await _collect_events(client, _make_request())

        text_deltas = [e for e in result if isinstance(e, ApiTextDeltaEvent)]
        assert len(text_deltas) == 3
        assert text_deltas[0].text == "Hello"
        assert text_deltas[1].text == " world"
        assert text_deltas[2].text == "!"

        complete = [e for e in result if isinstance(e, ApiMessageCompleteEvent)]
        assert len(complete) == 1
        assert complete[0].message.text == "Hello world!"


class TestToolUseMidStream:
    @pytest.mark.asyncio
    async def test_tool_use_mid_stream(self) -> None:
        """Two tool_use blocks are yielded mid-stream in order, before completion."""
        events = [
            # Tool 1: read_file
            MockEvent(
                type="content_block_start",
                index=0,
                content_block=MockContentBlock(type="tool_use", id="t1", name="read_file"),
            ),
            MockEvent(
                type="content_block_delta",
                index=0,
                delta=MockDelta(type="input_json_delta", partial_json='{"path":'),
            ),
            MockEvent(
                type="content_block_delta",
                index=0,
                delta=MockDelta(type="input_json_delta", partial_json=' "foo.txt"}'),
            ),
            MockEvent(type="content_block_stop", index=0),
            # Tool 2: write_file
            MockEvent(
                type="content_block_start",
                index=1,
                content_block=MockContentBlock(type="tool_use", id="t2", name="write_file"),
            ),
            MockEvent(
                type="content_block_delta",
                index=1,
                delta=MockDelta(type="input_json_delta", partial_json='{"path": "bar.txt"'),
            ),
            MockEvent(
                type="content_block_delta",
                index=1,
                delta=MockDelta(type="input_json_delta", partial_json=', "content": "hi"}'),
            ),
            MockEvent(type="content_block_stop", index=1),
            MockEvent(type="message_stop"),
        ]
        final = MockFinalMessage(
            content=[
                _make_tool_use_block("t1", "read_file", {"path": "foo.txt"}),
                _make_tool_use_block("t2", "write_file", {"path": "bar.txt", "content": "hi"}),
            ],
            usage=MockUsage(input_tokens=20, output_tokens=15),
        )

        client = _build_client()
        client._client.messages.stream = MagicMock(return_value=MockStream(events, final))

        result = await _collect_events(client, _make_request())

        tool_events = [e for e in result if isinstance(e, ApiToolUseDeltaEvent)]
        assert len(tool_events) == 2

        # t1 before t2
        assert tool_events[0].id == "t1"
        assert tool_events[0].name == "read_file"
        assert tool_events[0].input == {"path": "foo.txt"}

        assert tool_events[1].id == "t2"
        assert tool_events[1].name == "write_file"
        assert tool_events[1].input == {"path": "bar.txt", "content": "hi"}

        # Both tool events arrive before the complete event
        complete_idx = next(
            i for i, e in enumerate(result) if isinstance(e, ApiMessageCompleteEvent)
        )
        tool_indices = [i for i, e in enumerate(result) if isinstance(e, ApiToolUseDeltaEvent)]
        assert all(ti < complete_idx for ti in tool_indices)


class TestMixedTextAndTools:
    @pytest.mark.asyncio
    async def test_mixed_text_and_tools(self) -> None:
        """Text -> tool_use -> text yields events in correct interleaved order."""
        events = [
            # Text block 0
            MockEvent(
                type="content_block_start",
                index=0,
                content_block=MockContentBlock(type="text"),
            ),
            MockEvent(
                type="content_block_delta",
                index=0,
                delta=MockDelta(type="text_delta", text="Let me check."),
            ),
            MockEvent(type="content_block_stop", index=0),
            # Tool block 1
            MockEvent(
                type="content_block_start",
                index=1,
                content_block=MockContentBlock(type="tool_use", id="t1", name="search"),
            ),
            MockEvent(
                type="content_block_delta",
                index=1,
                delta=MockDelta(type="input_json_delta", partial_json='{"q": "test"}'),
            ),
            MockEvent(type="content_block_stop", index=1),
            # Text block 2
            MockEvent(
                type="content_block_start",
                index=2,
                content_block=MockContentBlock(type="text"),
            ),
            MockEvent(
                type="content_block_delta",
                index=2,
                delta=MockDelta(type="text_delta", text="Done."),
            ),
            MockEvent(type="content_block_stop", index=2),
            MockEvent(type="message_stop"),
        ]
        final = MockFinalMessage(
            content=[
                _make_text_block("Let me check."),
                _make_tool_use_block("t1", "search", {"q": "test"}),
                _make_text_block("Done."),
            ],
            usage=MockUsage(input_tokens=30, output_tokens=20),
        )

        client = _build_client()
        client._client.messages.stream = MagicMock(return_value=MockStream(events, final))

        result = await _collect_events(client, _make_request())

        # Filter to the meaningful event types
        meaningful = [
            e
            for e in result
            if isinstance(e, (ApiTextDeltaEvent, ApiToolUseDeltaEvent, ApiMessageCompleteEvent))
        ]
        assert isinstance(meaningful[0], ApiTextDeltaEvent)
        assert meaningful[0].text == "Let me check."
        assert isinstance(meaningful[1], ApiToolUseDeltaEvent)
        assert meaningful[1].name == "search"
        assert isinstance(meaningful[2], ApiTextDeltaEvent)
        assert meaningful[2].text == "Done."
        assert isinstance(meaningful[3], ApiMessageCompleteEvent)


class TestThinkingDelta:
    @pytest.mark.asyncio
    async def test_thinking_delta(self) -> None:
        """Thinking content yields ApiThinkingDeltaEvent."""
        events = [
            MockEvent(
                type="content_block_start",
                index=0,
                content_block=MockContentBlock(type="thinking"),
            ),
            MockEvent(
                type="content_block_delta",
                index=0,
                delta=MockDelta(type="thinking_delta", thinking="I need to reason about this."),
            ),
            MockEvent(type="content_block_stop", index=0),
            MockEvent(type="message_stop"),
        ]
        final = MockFinalMessage(
            content=[_make_thinking_block("I need to reason about this.")],
            usage=MockUsage(input_tokens=5, output_tokens=10),
        )

        client = _build_client()
        client._client.messages.stream = MagicMock(return_value=MockStream(events, final))

        result = await _collect_events(client, _make_request())

        thinking = [e for e in result if isinstance(e, ApiThinkingDeltaEvent)]
        assert len(thinking) == 1
        assert thinking[0].text == "I need to reason about this."


class TestEmptyToolInput:
    @pytest.mark.asyncio
    async def test_empty_tool_input(self) -> None:
        """Tool_use block with no input_json_delta events yields input={}."""
        events = [
            MockEvent(
                type="content_block_start",
                index=0,
                content_block=MockContentBlock(type="tool_use", id="t1", name="get_time"),
            ),
            # No input_json_delta events at all
            MockEvent(type="content_block_stop", index=0),
            MockEvent(type="message_stop"),
        ]
        final = MockFinalMessage(
            content=[_make_tool_use_block("t1", "get_time", {})],
            usage=MockUsage(input_tokens=8, output_tokens=4),
        )

        client = _build_client()
        client._client.messages.stream = MagicMock(return_value=MockStream(events, final))

        result = await _collect_events(client, _make_request())

        tool_events = [e for e in result if isinstance(e, ApiToolUseDeltaEvent)]
        assert len(tool_events) == 1
        assert tool_events[0].id == "t1"
        assert tool_events[0].name == "get_time"
        assert tool_events[0].input == {}


class TestRetryOn429:
    @pytest.mark.asyncio
    async def test_retry_on_429(self) -> None:
        """First call raises 429, second call succeeds — events from second call are yielded."""
        success_events = [
            MockEvent(
                type="content_block_start",
                index=0,
                content_block=MockContentBlock(type="text"),
            ),
            MockEvent(
                type="content_block_delta",
                index=0,
                delta=MockDelta(type="text_delta", text="ok"),
            ),
            MockEvent(type="content_block_stop", index=0),
            MockEvent(type="message_stop"),
        ]
        success_final = MockFinalMessage(
            content=[_make_text_block("ok")],
            usage=MockUsage(input_tokens=1, output_tokens=1),
        )

        # Build a 429 error
        mock_response = MagicMock()
        mock_response.status_code = 429
        mock_response.headers = {}
        error_429 = anthropic.APIStatusError(
            message="rate limited",
            response=mock_response,
            body=None,
        )

        call_count = 0

        def mock_stream(**kwargs: Any) -> MockStream:
            nonlocal call_count
            call_count += 1
            if call_count == 1:
                raise error_429
            return MockStream(success_events, success_final)

        client = _build_client()
        client._client.messages.stream = MagicMock(side_effect=mock_stream)

        with patch("models.clients.anthropic_native.asyncio.sleep", new_callable=AsyncMock):
            result = await _collect_events(client, _make_request())

        text_events = [e for e in result if isinstance(e, ApiTextDeltaEvent)]
        assert len(text_events) == 1
        assert text_events[0].text == "ok"
        assert call_count == 2


class TestNoRetryOn401:
    @pytest.mark.asyncio
    async def test_no_retry_on_401(self) -> None:
        """401 error raises AuthenticationFailure immediately with no retry."""
        mock_response = MagicMock()
        mock_response.status_code = 401
        mock_response.headers = {}
        error_401 = anthropic.APIStatusError(
            message="invalid api key",
            response=mock_response,
            body=None,
        )

        client = _build_client()
        client._client.messages.stream = MagicMock(side_effect=error_401)

        with pytest.raises(AuthenticationFailure):
            await _collect_events(client, _make_request())

        # Only called once — no retry
        assert client._client.messages.stream.call_count == 1


class TestRateLimitError:
    @pytest.mark.asyncio
    async def test_rate_limit_error(self) -> None:
        """All retries fail with 429 — raises RateLimitFailure."""
        mock_response = MagicMock()
        mock_response.status_code = 429
        mock_response.headers = {}
        error_429 = anthropic.APIStatusError(
            message="rate limited",
            response=mock_response,
            body=None,
        )

        client = _build_client()
        client._client.messages.stream = MagicMock(side_effect=error_429)

        with patch("models.clients.anthropic_native.asyncio.sleep", new_callable=AsyncMock):
            with pytest.raises(RateLimitFailure):
                await _collect_events(client, _make_request())

        # Initial attempt + MAX_RETRIES retries
        assert client._client.messages.stream.call_count == MAX_RETRIES + 1


class TestOutputSchemaStripped:
    @pytest.mark.asyncio
    async def test_output_schema_stripped(self) -> None:
        """Tools with output_schema have that key stripped before sending to the API."""
        events = [
            MockEvent(
                type="content_block_start",
                index=0,
                content_block=MockContentBlock(type="text"),
            ),
            MockEvent(
                type="content_block_delta",
                index=0,
                delta=MockDelta(type="text_delta", text="hi"),
            ),
            MockEvent(type="content_block_stop", index=0),
            MockEvent(type="message_stop"),
        ]
        final = MockFinalMessage(
            content=[_make_text_block("hi")],
            usage=MockUsage(input_tokens=5, output_tokens=2),
        )

        captured_kwargs: dict[str, Any] = {}

        def capture_stream(**kwargs: Any) -> MockStream:
            captured_kwargs.update(kwargs)
            return MockStream(events, final)

        tools_with_output_schema = [
            {
                "name": "get_weather",
                "description": "Get weather",
                "input_schema": {"type": "object", "properties": {}},
                "output_schema": {"type": "object", "properties": {"temp": {"type": "number"}}},
            },
        ]
        request = ApiMessageRequest(
            model="claude-sonnet-4-20250514",
            messages=[ConversationMessage.from_user_text("weather?")],
            tools=tools_with_output_schema,
        )

        client = _build_client()
        client._client.messages.stream = MagicMock(side_effect=capture_stream)

        await _collect_events(client, request)

        sent_tools = captured_kwargs["tools"]
        assert len(sent_tools) == 1
        assert "output_schema" not in sent_tools[0]
        assert sent_tools[0]["name"] == "get_weather"


class TestUsageSnapshot:
    @pytest.mark.asyncio
    async def test_usage_snapshot(self) -> None:
        """ApiMessageCompleteEvent carries correct UsageSnapshot from the final message."""
        events = [
            MockEvent(
                type="content_block_start",
                index=0,
                content_block=MockContentBlock(type="text"),
            ),
            MockEvent(
                type="content_block_delta",
                index=0,
                delta=MockDelta(type="text_delta", text="x"),
            ),
            MockEvent(type="content_block_stop", index=0),
            MockEvent(type="message_stop"),
        ]
        final = MockFinalMessage(
            content=[_make_text_block("x")],
            usage=MockUsage(input_tokens=100, output_tokens=50),
        )

        client = _build_client()
        client._client.messages.stream = MagicMock(return_value=MockStream(events, final))

        result = await _collect_events(client, _make_request())

        complete = [e for e in result if isinstance(e, ApiMessageCompleteEvent)]
        assert len(complete) == 1
        assert complete[0].usage.input_tokens == 100
        assert complete[0].usage.output_tokens == 50
        assert complete[0].usage.total_tokens == 150
        assert complete[0].stop_reason == "end_turn"
