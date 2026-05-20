"""Plan §A7 — refresh-on-401 retry-once.

Two cases per S3 acceptance criteria:

* (a) refresh-True: first attempt emits 2 deltas then 401; strategy.refresh()
      returns True; SDK rebuilt; retry attempted; full stream re-emitted.
* (b) refresh-False: first attempt raises 401; strategy.refresh() returns
      False; NO retry; AuthenticationFailure surfaces immediately.
"""

from __future__ import annotations

from typing import Any
from unittest.mock import MagicMock

import anthropic
import pytest

from providers.clients.anthropic_native import AnthropicClient
from providers.errors import AuthenticationFailure
from providers.types import (
    ApiMessageCompleteEvent,
    ApiMessageRequest,
    ApiTextDeltaEvent,
)
from message import ConversationMessage


def _make_api_status_error(status_code: int, message: str) -> anthropic.APIStatusError:
    mock_response = MagicMock()
    mock_response.status_code = status_code
    mock_response.headers = {}
    return anthropic.APIStatusError(message=message, response=mock_response, body=None)


def _make_request() -> ApiMessageRequest:
    return ApiMessageRequest(
        model="claude-sonnet-4-20250514",
        messages=[ConversationMessage.from_user_text("hi")],
    )


class _MockDelta:
    def __init__(self, type: str, **kwargs: Any) -> None:
        self.type = type
        for k, v in kwargs.items():
            setattr(self, k, v)


class _MockContentBlock:
    def __init__(self, type: str, **kwargs: Any) -> None:
        self.type = type
        for k, v in kwargs.items():
            setattr(self, k, v)


class _MockEvent:
    def __init__(self, type: str, **kwargs: Any) -> None:
        self.type = type
        for k, v in kwargs.items():
            setattr(self, k, v)


class _MockUsage:
    input_tokens = 1
    output_tokens = 1


class _MockFinalMessage:
    content = [_MockContentBlock(type="text", text="ok")]
    usage = _MockUsage()
    stop_reason = "end_turn"


class _MockStream:
    def __init__(self, events: list[_MockEvent]) -> None:
        self._events = events

    async def __aenter__(self) -> "_MockStream":
        return self

    async def __aexit__(self, *args: Any) -> None:
        return None

    async def __aiter__(self):
        for ev in self._events:
            yield ev

    async def get_final_message(self) -> _MockFinalMessage:
        return _MockFinalMessage()


def _ok_events(text: str) -> list[_MockEvent]:
    return [
        _MockEvent(type="content_block_start", index=0, content_block=_MockContentBlock(type="text")),
        _MockEvent(type="content_block_delta", index=0, delta=_MockDelta(type="text_delta", text=text)),
        _MockEvent(type="content_block_stop", index=0),
        _MockEvent(type="message_stop"),
    ]


def _build_refresh_strategy(refresh_returns: bool) -> MagicMock:
    """Build a fake strategy whose refresh() returns the given bool and
    whose get_auth_kwargs is callable for the rebuild step."""
    from providers.auth_strategy import (
        CLAUDE_OAUTH_DEFAULT_HEADERS,
        LLM_CLIENT_MODE_CODING_PLAN,
    )

    state = {"token": "sk-ant-oat01-OLD"}

    def get_auth_kwargs() -> dict[str, object]:
        return {
            "auth_token": state["token"],
            "default_headers": dict(CLAUDE_OAUTH_DEFAULT_HEADERS),
        }

    def refresh() -> bool:
        if refresh_returns:
            state["token"] = "sk-ant-oat01-NEW"
        return refresh_returns

    strat = MagicMock()
    strat.llm_client_mode = LLM_CLIENT_MODE_CODING_PLAN
    strat.get_auth_kwargs.side_effect = get_auth_kwargs
    strat.refresh.side_effect = refresh
    return strat


def _make_stable_mock_client() -> MagicMock:
    """Build a fresh mock anthropic.AsyncAnthropic instance with a stub
    .messages.stream that the test will override per-call."""
    mock_sdk = MagicMock()
    mock_sdk.messages = MagicMock()
    return mock_sdk


@pytest.mark.asyncio
async def test_refresh_true_retries_once_with_new_token(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """The rebuilt SDK shares the same stream mock so the rebuild itself
    is transparent to the assertion — what matters is that refresh was
    called, a second attempt happened, and the second attempt's events
    were yielded."""
    mock_sdk = _make_stable_mock_client()
    # AnthropicClient.__init__ + _build_sdk_client both call
    # anthropic.AsyncAnthropic(...); return the SAME mock instance both
    # times so the refresh-rebuild keeps the staged side_effect alive.
    monkeypatch.setattr(
        "providers.clients.anthropic_native.anthropic.AsyncAnthropic",
        lambda **_kwargs: mock_sdk,
    )

    strat = _build_refresh_strategy(refresh_returns=True)
    client = AnthropicClient(auth_strategy=strat)

    error_401 = _make_api_status_error(401, "token expired")
    call_count = {"n": 0}

    def mock_stream(**_kwargs: Any) -> Any:
        call_count["n"] += 1
        if call_count["n"] == 1:
            class _Aborting:
                async def __aenter__(self) -> "_Aborting":
                    return self

                async def __aexit__(self, *args: Any) -> None:
                    return None

                async def __aiter__(self):
                    yield _MockEvent(
                        type="content_block_start",
                        index=0,
                        content_block=_MockContentBlock(type="text"),
                    )
                    yield _MockEvent(
                        type="content_block_delta",
                        index=0,
                        delta=_MockDelta(type="text_delta", text="part1"),
                    )
                    yield _MockEvent(
                        type="content_block_delta",
                        index=0,
                        delta=_MockDelta(type="text_delta", text="part2"),
                    )
                    raise error_401

                async def get_final_message(self) -> _MockFinalMessage:
                    return _MockFinalMessage()

            return _Aborting()
        return _MockStream(_ok_events("retry_ok"))

    mock_sdk.messages.stream = MagicMock(side_effect=mock_stream)

    events: list[Any] = []
    async for ev in client.stream_message(_make_request()):
        events.append(ev)

    assert strat.refresh.call_count == 1
    text_deltas = [e for e in events if isinstance(e, ApiTextDeltaEvent)]
    assert [d.text for d in text_deltas] == ["part1", "part2", "retry_ok"]
    completes = [e for e in events if isinstance(e, ApiMessageCompleteEvent)]
    assert len(completes) == 1
    # Two SDK constructions (init + refresh-rebuild) imply two
    # get_auth_kwargs calls.
    assert strat.get_auth_kwargs.call_count >= 2


@pytest.mark.asyncio
async def test_refresh_false_raises_without_retry(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    mock_sdk = _make_stable_mock_client()
    monkeypatch.setattr(
        "providers.clients.anthropic_native.anthropic.AsyncAnthropic",
        lambda **_kwargs: mock_sdk,
    )

    strat = _build_refresh_strategy(refresh_returns=False)
    client = AnthropicClient(auth_strategy=strat)

    mock_sdk.messages.stream = MagicMock(
        side_effect=_make_api_status_error(401, "invalid token")
    )

    with pytest.raises(AuthenticationFailure):
        async for _ in client.stream_message(_make_request()):
            pass

    assert strat.refresh.call_count == 1
    # No retry: only one SDK call total.
    assert mock_sdk.messages.stream.call_count == 1
