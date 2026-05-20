"""Plan §S4 — A17 Codex-side coding_plan_mode_error structured log."""

from __future__ import annotations

import base64
import json
import logging
from pathlib import Path
from typing import Any

import pytest

from providers.clients.coding_plan.codex import CodexResponsesClient
from providers.errors import AuthenticationFailure, RequestFailure
from providers.types import ApiMessageRequest
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


class _FakeStreamResponse:
    def __init__(
        self,
        *,
        status_code: int,
        body: bytes = b"",
        headers: dict[str, str] | None = None,
    ) -> None:
        self.status_code = status_code
        self._body = body
        self.headers = headers or {}

    async def __aenter__(self) -> "_FakeStreamResponse":
        return self

    async def __aexit__(self, *args: Any) -> None:
        return None

    async def aiter_lines(self):
        if False:
            yield ""

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


def _install_fake_httpx(
    monkeypatch: pytest.MonkeyPatch, response: _FakeStreamResponse
) -> None:
    fake = _FakeHttpxClient(response)
    monkeypatch.setattr(
        "providers.clients.coding_plan.codex.httpx.AsyncClient",
        lambda *_a, **_kw: fake,
    )


def _request() -> ApiMessageRequest:
    return ApiMessageRequest(
        model="gpt-5.5", messages=[ConversationMessage.from_user_text("x")]
    )


@pytest.mark.asyncio
async def test_codex_401_emits_auth_401(
    monkeypatch: pytest.MonkeyPatch,
    caplog: pytest.LogCaptureFixture,
    _fake_codex_auth: dict[str, Path],
) -> None:
    _install_fake_httpx(
        monkeypatch,
        _FakeStreamResponse(
            status_code=401, body=b'{"error":{"message":"invalid token"}}'
        ),
    )
    client = CodexResponsesClient(db_kwargs=_fake_codex_auth)

    caplog.set_level(logging.ERROR, logger="providers.clients.coding_plan.codex")
    with pytest.raises(AuthenticationFailure):
        async for _ in client.stream_message(_request()):
            pass

    records = [r for r in caplog.records if r.message == "coding_plan_mode_error"]
    assert len(records) == 1
    rec = records[0]
    assert getattr(rec, "provider", None) == "codex"
    assert getattr(rec, "error_type", None) == "auth_401"


@pytest.mark.asyncio
async def test_codex_schema_reject_emits_schema_rejected(
    monkeypatch: pytest.MonkeyPatch,
    caplog: pytest.LogCaptureFixture,
    _fake_codex_auth: dict[str, Path],
) -> None:
    _install_fake_httpx(
        monkeypatch,
        _FakeStreamResponse(
            status_code=400,
            body=b'{"error":{"message":"Tool schema rejected: parameters.additionalProperties not allowed"}}',
        ),
    )
    client = CodexResponsesClient(db_kwargs=_fake_codex_auth)

    caplog.set_level(logging.ERROR, logger="providers.clients.coding_plan.codex")
    with pytest.raises(RequestFailure):
        async for _ in client.stream_message(_request()):
            pass

    records = [r for r in caplog.records if r.message == "coding_plan_mode_error"]
    assert len(records) == 1
    assert getattr(records[0], "error_type", None) == "schema_rejected"


@pytest.mark.asyncio
async def test_codex_model_rejected_emits_model_rejected(
    monkeypatch: pytest.MonkeyPatch,
    caplog: pytest.LogCaptureFixture,
    _fake_codex_auth: dict[str, Path],
) -> None:
    _install_fake_httpx(
        monkeypatch,
        _FakeStreamResponse(
            status_code=400,
            body=b'{"error":{"message":"model gpt-5-codex is not supported when using Codex with a ChatGPT account"}}',
        ),
    )
    client = CodexResponsesClient(db_kwargs=_fake_codex_auth)

    caplog.set_level(logging.ERROR, logger="providers.clients.coding_plan.codex")
    with pytest.raises(RequestFailure):
        async for _ in client.stream_message(_request()):
            pass

    records = [r for r in caplog.records if r.message == "coding_plan_mode_error"]
    assert len(records) == 1
    assert getattr(records[0], "error_type", None) == "model_rejected"
