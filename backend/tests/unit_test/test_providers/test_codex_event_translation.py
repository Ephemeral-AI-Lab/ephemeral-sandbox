"""Plan §S4 — Codex SSE → ApiStreamEvent translation.

Exercises the 5+ event types per the v9.2 mapping table.

We mock httpx.AsyncClient so the stream returns a canned SSE byte sequence
and the client's event-translation logic is driven without network I/O.
"""

from __future__ import annotations

import base64
import json
from pathlib import Path
from typing import Any

import pytest

from providers.clients.coding_plan.codex import CodexResponsesClient
from providers.types import (
    ApiMessageCompleteEvent,
    ApiMessageRequest,
    ApiTextDeltaEvent,
    ApiThinkingDeltaEvent,
    ApiToolUseDeltaEvent,
)
from message import ConversationMessage


def _b64url(data: bytes) -> str:
    return base64.urlsafe_b64encode(data).rstrip(b"=").decode("ascii")


def _fake_id_token() -> str:
    payload = {"https://api.openai.com/auth": {"chatgpt_account_id": "acct-x"}}
    header = _b64url(json.dumps({"alg": "RS256", "typ": "JWT"}).encode())
    return f"{header}.{_b64url(json.dumps(payload).encode())}.sig"


@pytest.fixture()
def _fake_codex_auth(tmp_path: Path) -> dict[str, Path]:
    auth_path = tmp_path / "auth.json"
    auth_path.write_text(
        json.dumps(
            {
                "tokens": {
                    "access_token": "codex_access_FAKE",
                    "id_token": _fake_id_token(),
                }
            }
        ),
        encoding="utf-8",
    )
    return {"auth_path": auth_path, "config_path": tmp_path / "config.toml"}


def _sse_line(event_dict: dict[str, Any]) -> str:
    return f"data: {json.dumps(event_dict)}"


class _FakeStreamResponse:
    """Stand-in for httpx.Response under streaming."""

    def __init__(
        self,
        lines: list[str],
        *,
        status_code: int = 200,
        headers: dict[str, str] | None = None,
    ) -> None:
        self._lines = lines
        self.status_code = status_code
        self.headers = headers or {}

    async def __aenter__(self) -> "_FakeStreamResponse":
        return self

    async def __aexit__(self, *args: Any) -> None:
        return None

    async def aiter_lines(self):
        for line in self._lines:
            yield line

    async def aread(self) -> bytes:
        return "\n".join(self._lines).encode("utf-8")


class _FakeHttpxClient:
    def __init__(self, response: _FakeStreamResponse) -> None:
        self._response = response

    async def __aenter__(self) -> "_FakeHttpxClient":
        return self

    async def __aexit__(self, *args: Any) -> None:
        return None

    def stream(self, *_args: Any, **_kwargs: Any) -> _FakeStreamResponse:
        return self._response


def _install_fake_httpx(
    monkeypatch: pytest.MonkeyPatch, response: _FakeStreamResponse
) -> None:
    fake = _FakeHttpxClient(response)
    monkeypatch.setattr(
        "providers.clients.coding_plan.codex.httpx.AsyncClient",
        lambda *_a, **_kw: fake,
    )


@pytest.mark.asyncio
async def test_translates_text_delta_to_api_text_delta(
    monkeypatch: pytest.MonkeyPatch, _fake_codex_auth: dict[str, Path]
) -> None:
    lines = [
        _sse_line({"type": "response.created", "response": {"id": "resp_1"}}),
        _sse_line({"type": "response.output_text.delta", "delta": "Hello "}),
        _sse_line({"type": "response.output_text.delta", "delta": "world"}),
        _sse_line(
            {
                "type": "response.completed",
                "response": {
                    "id": "resp_1",
                    "usage": {"input_tokens": 5, "output_tokens": 2},
                    "stop_reason": "end_turn",
                },
            }
        ),
    ]
    _install_fake_httpx(monkeypatch, _FakeStreamResponse(lines))

    client = CodexResponsesClient(db_kwargs=_fake_codex_auth)
    request = ApiMessageRequest(
        model="gpt-5.5",
        messages=[ConversationMessage.from_user_text("greet")],
    )

    events: list[Any] = [ev async for ev in client.stream_message(request)]
    text_deltas = [e for e in events if isinstance(e, ApiTextDeltaEvent)]
    assert [d.text for d in text_deltas] == ["Hello ", "world"]
    completes = [e for e in events if isinstance(e, ApiMessageCompleteEvent)]
    assert len(completes) == 1
    assert completes[0].usage.input_tokens == 5
    assert completes[0].usage.output_tokens == 2
    assert completes[0].stop_reason == "end_turn"


@pytest.mark.asyncio
async def test_translates_reasoning_summary_to_thinking(
    monkeypatch: pytest.MonkeyPatch, _fake_codex_auth: dict[str, Path]
) -> None:
    lines = [
        _sse_line({"type": "response.created", "response": {"id": "resp_2"}}),
        _sse_line(
            {
                "type": "response.reasoning_summary_text.delta",
                "delta": "I should read the file first.",
            }
        ),
        _sse_line(
            {
                "type": "response.completed",
                "response": {"id": "resp_2", "usage": {}},
            }
        ),
    ]
    _install_fake_httpx(monkeypatch, _FakeStreamResponse(lines))

    client = CodexResponsesClient(db_kwargs=_fake_codex_auth)
    request = ApiMessageRequest(
        model="gpt-5.5",
        messages=[ConversationMessage.from_user_text("think")],
    )

    events = [ev async for ev in client.stream_message(request)]
    thinking = [e for e in events if isinstance(e, ApiThinkingDeltaEvent)]
    assert len(thinking) == 1
    assert thinking[0].text == "I should read the file first."


@pytest.mark.asyncio
async def test_translates_function_call_lifecycle_to_tool_use(
    monkeypatch: pytest.MonkeyPatch, _fake_codex_auth: dict[str, Path]
) -> None:
    lines = [
        _sse_line({"type": "response.created", "response": {"id": "resp_3"}}),
        _sse_line(
            {
                "type": "response.output_item.added",
                "item": {
                    "id": "item_1",
                    "type": "function_call",
                    "call_id": "call_1",
                    "name": "read_file",
                },
            }
        ),
        _sse_line(
            {
                "type": "response.function_call_arguments.delta",
                "item_id": "item_1",
                "delta": '{"path":',
            }
        ),
        _sse_line(
            {
                "type": "response.function_call_arguments.delta",
                "item_id": "item_1",
                "delta": ' "foo.txt"}',
            }
        ),
        _sse_line(
            {
                "type": "response.function_call_arguments.done",
                "item_id": "item_1",
            }
        ),
        _sse_line(
            {
                "type": "response.completed",
                "response": {"id": "resp_3", "usage": {}},
            }
        ),
    ]
    _install_fake_httpx(monkeypatch, _FakeStreamResponse(lines))

    client = CodexResponsesClient(db_kwargs=_fake_codex_auth)
    request = ApiMessageRequest(
        model="gpt-5.5",
        messages=[ConversationMessage.from_user_text("call_tool")],
    )

    events = [ev async for ev in client.stream_message(request)]
    tools = [e for e in events if isinstance(e, ApiToolUseDeltaEvent)]
    assert len(tools) == 1
    assert tools[0].id == "call_1"
    assert tools[0].name == "read_file"
    assert tools[0].input == {"path": "foo.txt"}


@pytest.mark.asyncio
async def test_in_progress_and_done_events_are_no_op(
    monkeypatch: pytest.MonkeyPatch, _fake_codex_auth: dict[str, Path]
) -> None:
    """response.in_progress + response.output_item.done must not break the
    stream — they're informational and have no ApiStreamEvent equivalent."""
    lines = [
        _sse_line({"type": "response.created", "response": {"id": "resp_4"}}),
        _sse_line({"type": "response.in_progress", "response": {"id": "resp_4"}}),
        _sse_line({"type": "response.output_text.delta", "delta": "x"}),
        _sse_line(
            {
                "type": "response.output_item.done",
                "item": {"id": "item_1", "type": "message"},
            }
        ),
        _sse_line(
            {
                "type": "response.completed",
                "response": {"id": "resp_4", "usage": {}},
            }
        ),
    ]
    _install_fake_httpx(monkeypatch, _FakeStreamResponse(lines))

    client = CodexResponsesClient(db_kwargs=_fake_codex_auth)
    events = [
        ev
        async for ev in client.stream_message(
            ApiMessageRequest(
                model="gpt-5.5",
                messages=[ConversationMessage.from_user_text("x")],
            )
        )
    ]
    assert any(isinstance(e, ApiTextDeltaEvent) for e in events)
    assert any(isinstance(e, ApiMessageCompleteEvent) for e in events)
