"""Plan §A17 — Anthropic plan-mode structured error logging.

Two cases per S1.4 acceptance criteria: 401 → auth_401, 429 → rate_limit_429.
Asserts ``log.error("coding_plan_mode_error", extra={...})`` with the post-translation
category, using the API-mode regression check separately to confirm the
log does NOT fire when ``llm_client_mode != "coding_plan_mode"``.
"""

from __future__ import annotations

import logging
from typing import Any
from unittest.mock import MagicMock, patch

import anthropic
import pytest

from providers.clients.anthropic_native import AnthropicClient
from providers.errors import AuthenticationFailure, RateLimitFailure
from providers.types import ApiMessageRequest
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


def _build_plan_client() -> AnthropicClient:
    """Build a coding-plan-mode client without touching the keychain."""
    from providers.auth_strategy import (
        CLAUDE_OAUTH_DEFAULT_HEADERS,
        CLAUDE_OAUTH_SYSTEM_PREFIX,
        LLM_CLIENT_MODE_CODING_PLAN,
    )

    fake_strategy = MagicMock()
    fake_strategy.llm_client_mode = LLM_CLIENT_MODE_CODING_PLAN
    fake_strategy.get_auth_kwargs.return_value = {
        "auth_token": "sk-ant-oat01-FAKE",
        "default_headers": dict(CLAUDE_OAUTH_DEFAULT_HEADERS),
    }
    fake_strategy.refresh.return_value = False
    return AnthropicClient(
        auth_strategy=fake_strategy, system_prefix=CLAUDE_OAUTH_SYSTEM_PREFIX
    )


async def _drain(client: AnthropicClient) -> list[Any]:
    out: list[Any] = []
    async for ev in client.stream_message(_make_request()):
        out.append(ev)
    return out


@pytest.mark.asyncio
async def test_plan_mode_logs_auth_401(caplog: pytest.LogCaptureFixture) -> None:
    client = _build_plan_client()
    client._client.messages.stream = MagicMock(  # type: ignore[attr-defined]
        side_effect=_make_api_status_error(401, "invalid token")
    )

    caplog.set_level(logging.ERROR, logger="providers.clients.anthropic_native")
    with pytest.raises(AuthenticationFailure):
        await _drain(client)

    records = [r for r in caplog.records if r.message == "coding_plan_mode_error"]
    assert len(records) == 1, f"expected one coding_plan_mode_error log, got {records}"
    record = records[0]
    assert getattr(record, "provider", None) == "anthropic"
    assert getattr(record, "error_type", None) == "auth_401"


@pytest.mark.asyncio
async def test_plan_mode_logs_rate_limit_429(caplog: pytest.LogCaptureFixture) -> None:
    client = _build_plan_client()
    client._client.messages.stream = MagicMock(  # type: ignore[attr-defined]
        side_effect=_make_api_status_error(429, "rate limited")
    )

    caplog.set_level(logging.ERROR, logger="providers.clients.anthropic_native")
    with patch(
        "providers.clients.anthropic_native.asyncio.sleep",
        new=_make_async_noop(),
    ):
        with pytest.raises(RateLimitFailure):
            await _drain(client)

    records = [r for r in caplog.records if r.message == "coding_plan_mode_error"]
    # One final emission after MAX_RETRIES exhausted (intermediate retries
    # log warnings but do NOT emit coding_plan_mode_error — see _emit_coding_plan_mode_error
    # call site: only the terminal raise emits).
    assert len(records) == 1, f"expected one terminal coding_plan_mode_error log, got {records}"
    record = records[0]
    assert getattr(record, "provider", None) == "anthropic"
    assert getattr(record, "error_type", None) == "rate_limit_429"


@pytest.mark.asyncio
async def test_api_mode_does_not_emit_coding_plan_mode_error(
    caplog: pytest.LogCaptureFixture,
) -> None:
    """Regression: api_mode client must NOT emit coding_plan_mode_error log lines."""
    client = AnthropicClient(api_key="sk-x")
    client._client.messages.stream = MagicMock(  # type: ignore[attr-defined]
        side_effect=_make_api_status_error(401, "bad key")
    )
    caplog.set_level(logging.ERROR, logger="providers.clients.anthropic_native")
    with pytest.raises(AuthenticationFailure):
        await _drain(client)
    records = [r for r in caplog.records if r.message == "coding_plan_mode_error"]
    assert records == []


def _make_async_noop():
    async def _noop(_seconds: float) -> None:
        return None

    return _noop
