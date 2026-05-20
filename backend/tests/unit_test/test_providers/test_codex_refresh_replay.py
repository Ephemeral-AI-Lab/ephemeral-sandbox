"""Plan §A7 — Codex refresh-on-401 retry-once symmetry.

Two cases mirroring the Anthropic-side S3 pattern:

* (a) refresh-True: first httpx attempt returns 401; ``_refresh_credentials``
      finds NEW tokens; retry attempt yields a canned 200 SSE stream;
      full event sequence reaches the consumer.
* (b) refresh-False: first httpx attempt returns 401; ``_refresh_credentials``
      finds SAME tokens; ``AuthenticationFailure`` raised; NO retry.
"""

from __future__ import annotations

import base64
import json
from pathlib import Path
from typing import Any

import pytest

from providers.clients.coding_plan.codex import CodexResponsesClient
from providers.errors import AuthenticationFailure
from providers.types import (
    ApiMessageCompleteEvent,
    ApiMessageRequest,
    ApiTextDeltaEvent,
)
from message import ConversationMessage


def _b64url(data: bytes) -> str:
    return base64.urlsafe_b64encode(data).rstrip(b"=").decode("ascii")


def _fake_id_token(account_id: str) -> str:
    """JWT carrying the Auth0-namespaced ``chatgpt_account_id`` claim."""
    payload = {"https://api.openai.com/auth": {"chatgpt_account_id": account_id}}
    header = _b64url(json.dumps({"alg": "RS256", "typ": "JWT"}).encode())
    body = _b64url(json.dumps(payload).encode())
    return f"{header}.{body}.sig"


@pytest.fixture()
def _codex_auth_paths(tmp_path: Path) -> dict[str, Path]:
    """Stub paths for the constructor; ``_load_codex_auth`` is monkeypatched
    per test so the real auth.json is never read. Missing config.toml
    defaults the model to ``gpt-5.5``.
    """
    return {
        "auth_path": tmp_path / "auth.json",
        "config_path": tmp_path / "config.toml",
    }


class _FakeStreamResponse:
    """Stand-in for httpx.Response under streaming."""

    def __init__(
        self,
        lines: list[str] | None = None,
        *,
        status_code: int = 200,
        body: bytes = b"",
        headers: dict[str, str] | None = None,
    ) -> None:
        self._lines = lines or []
        self.status_code = status_code
        self._body = body
        self.headers = headers or {}

    async def __aenter__(self) -> "_FakeStreamResponse":
        return self

    async def __aexit__(self, *args: Any) -> None:
        return None

    async def aiter_lines(self):
        for line in self._lines:
            yield line

    async def aread(self) -> bytes:
        return self._body


class _FakeHttpxClient:
    def __init__(self, response: _FakeStreamResponse) -> None:
        self._response = response

    async def __aenter__(self) -> "_FakeHttpxClient":
        return self

    async def __aexit__(self, *args: Any) -> None:
        return None

    def stream(self, *_args: Any, **_kwargs: Any) -> _FakeStreamResponse:
        return self._response


class _StagedHttpxClients:
    """Factory yielding prepared httpx clients sequentially.

    Each ``async with httpx.AsyncClient(...)`` invocation pops the next
    queued response. Records the call count so tests can assert whether
    a retry attempt fired.
    """

    def __init__(self, responses: list[_FakeStreamResponse]) -> None:
        self._queue = list(responses)
        self.calls = 0

    def __call__(self, *_args: Any, **_kwargs: Any) -> _FakeHttpxClient:
        self.calls += 1
        if not self._queue:
            raise AssertionError(
                "Unexpected extra httpx.AsyncClient() invocation"
            )
        return _FakeHttpxClient(self._queue.pop(0))


def _sse_line(event_dict: dict[str, Any]) -> str:
    return f"data: {json.dumps(event_dict)}"


def _ok_sse_lines() -> list[str]:
    return [
        _sse_line({"type": "response.created", "response": {"id": "resp_retry"}}),
        _sse_line(
            {"type": "response.output_text.delta", "delta": "after-refresh"}
        ),
        _sse_line(
            {
                "type": "response.completed",
                "response": {
                    "id": "resp_retry",
                    "usage": {"input_tokens": 1, "output_tokens": 1},
                    "stop_reason": "end_turn",
                },
            }
        ),
    ]


def _request() -> ApiMessageRequest:
    return ApiMessageRequest(
        model="gpt-5.5",
        messages=[ConversationMessage.from_user_text("hi")],
    )


@pytest.mark.asyncio
async def test_refresh_true_retries_once_with_new_token(
    monkeypatch: pytest.MonkeyPatch,
    _codex_auth_paths: dict[str, Path],
) -> None:
    """First attempt → 401; refresh returns True (NEW access + NEW account_id);
    second attempt → 200 SSE; the retry's event stream reaches the consumer."""
    auth_calls: list[Path] = []
    staged_auth = [
        ("OLD-access", _fake_id_token("OLD-acct")),
        ("NEW-access", _fake_id_token("NEW-acct")),
    ]

    def fake_load(auth_path: Path) -> tuple[str, str]:
        idx = len(auth_calls)
        auth_calls.append(auth_path)
        return staged_auth[idx]

    monkeypatch.setattr(
        CodexResponsesClient,
        "_load_codex_auth",
        staticmethod(fake_load),
    )

    staged_http = _StagedHttpxClients(
        [
            _FakeStreamResponse(status_code=401, body=b'{"error":"expired"}'),
            _FakeStreamResponse(_ok_sse_lines()),
        ]
    )
    monkeypatch.setattr(
        "providers.clients.coding_plan.codex.httpx.AsyncClient",
        staged_http,
    )

    client = CodexResponsesClient(db_kwargs=_codex_auth_paths)
    # The constructor consumed the first staged auth.
    assert len(auth_calls) == 1
    assert client._access_token == "OLD-access"
    assert client._chatgpt_account_id == "OLD-acct"

    events: list[Any] = [ev async for ev in client.stream_message(_request())]

    # _refresh_credentials() fired once during stream_message — the
    # cumulative auth-load count is 2 (init + refresh).
    assert len(auth_calls) == 2
    # Two httpx.AsyncClient() invocations: the 401 attempt + the retry.
    assert staged_http.calls == 2
    # The retry's SSE deltas reached the consumer.
    text_deltas = [e for e in events if isinstance(e, ApiTextDeltaEvent)]
    assert [d.text for d in text_deltas] == ["after-refresh"]
    completes = [e for e in events if isinstance(e, ApiMessageCompleteEvent)]
    assert len(completes) == 1
    # In-place mutation by _refresh_credentials rotated both fields.
    assert client._access_token == "NEW-access"
    assert client._chatgpt_account_id == "NEW-acct"


@pytest.mark.asyncio
async def test_refresh_false_raises_without_retry(
    monkeypatch: pytest.MonkeyPatch,
    _codex_auth_paths: dict[str, Path],
) -> None:
    """First attempt → 401; refresh reloads the SAME tokens (no vendor-side
    rotation); ``_refresh_credentials`` returns False; ``AuthenticationFailure``
    raises immediately with no second httpx attempt."""
    auth_calls: list[Path] = []
    same_id_token = _fake_id_token("OLD-acct")

    def fake_load(auth_path: Path) -> tuple[str, str]:
        auth_calls.append(auth_path)
        return ("OLD-access", same_id_token)

    monkeypatch.setattr(
        CodexResponsesClient,
        "_load_codex_auth",
        staticmethod(fake_load),
    )

    staged_http = _StagedHttpxClients(
        [_FakeStreamResponse(status_code=401, body=b'{"error":"expired"}')]
    )
    monkeypatch.setattr(
        "providers.clients.coding_plan.codex.httpx.AsyncClient",
        staged_http,
    )

    client = CodexResponsesClient(db_kwargs=_codex_auth_paths)
    assert len(auth_calls) == 1

    with pytest.raises(AuthenticationFailure):
        async for _ in client.stream_message(_request()):
            pass

    # _refresh_credentials() fired once and observed unchanged tokens.
    assert len(auth_calls) == 2
    # Only the initial 401 attempt — no retry stream.
    assert staged_http.calls == 1
    # In-place mutation skipped — fields stayed on OLD values.
    assert client._access_token == "OLD-access"
    assert client._chatgpt_account_id == "OLD-acct"
