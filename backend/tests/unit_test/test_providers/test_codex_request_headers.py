"""Plan §A4 — Codex request shape (5 headers + FLAT tools + no max_output_tokens)."""

from __future__ import annotations

import base64
import json
from pathlib import Path

import pytest

from providers.clients.coding_plan.codex import (
    CODEX_DEFAULT_MODEL,
    CodexResponsesClient,
)
from providers.types import ApiMessageRequest
from message import ConversationMessage


def _b64url(data: bytes) -> str:
    return base64.urlsafe_b64encode(data).rstrip(b"=").decode("ascii")


def _fake_id_token(account_id: str = "acct-fake") -> str:
    payload = {"https://api.openai.com/auth": {"chatgpt_account_id": account_id}}
    header = _b64url(json.dumps({"alg": "RS256", "typ": "JWT"}).encode())
    body = _b64url(json.dumps(payload).encode())
    return f"{header}.{body}.sig"


@pytest.fixture()
def _fake_codex_auth(tmp_path: Path) -> dict[str, Path]:
    auth_path = tmp_path / "auth.json"
    auth_path.write_text(
        json.dumps(
            {
                "tokens": {
                    "access_token": "codex_access_FAKE_TOKEN",
                    "id_token": _fake_id_token(),
                    "refresh_token": "codex_refresh_FAKE",
                }
            }
        ),
        encoding="utf-8",
    )
    config_path = tmp_path / "config.toml"
    return {"auth_path": auth_path, "config_path": config_path}


def test_build_headers_contains_all_five_allowlist_headers(
    _fake_codex_auth: dict[str, Path]
) -> None:
    client = CodexResponsesClient(db_kwargs=_fake_codex_auth)
    headers = client.build_headers()

    assert headers["Authorization"] == "Bearer codex_access_FAKE_TOKEN"
    assert headers["ChatGPT-Account-Id"] == "acct-fake"
    assert headers["originator"] == "codex_cli_rs"
    assert headers["User-Agent"] == "codex_cli_rs/0.125"
    assert headers["OpenAI-Beta"] == "responses=experimental"
    assert headers["Content-Type"] == "application/json"


def test_build_body_omits_max_output_tokens_and_uses_flat_tools(
    _fake_codex_auth: dict[str, Path]
) -> None:
    client = CodexResponsesClient(db_kwargs=_fake_codex_auth)
    request = ApiMessageRequest(
        model=CODEX_DEFAULT_MODEL,
        messages=[ConversationMessage.from_user_text("hi")],
        system_prompt="be helpful",
        tools=[
            {
                "name": "read_file",
                "description": "Read a file",
                "input_schema": {
                    "type": "object",
                    "properties": {"path": {"type": "string"}},
                    "required": ["path"],
                },
                "output_schema": {"type": "string"},
            }
        ],
    )

    body = client.build_body(request)

    assert body["model"] == CODEX_DEFAULT_MODEL
    assert body["instructions"] == "be helpful"
    assert body["stream"] is True
    assert body["store"] is False
    assert "max_output_tokens" not in body

    assert len(body["tools"]) == 1
    tool = body["tools"][0]
    # FLAT envelope: type+name+description+parameters at top level.
    assert tool["type"] == "function"
    assert tool["name"] == "read_file"
    assert tool["description"] == "Read a file"
    assert tool["parameters"]["type"] == "object"
    # No nested function-key shape (Chat-Completions style).
    assert "function" not in tool


def test_model_falls_back_to_default_when_kwargs_and_config_missing(
    _fake_codex_auth: dict[str, Path]
) -> None:
    client = CodexResponsesClient(db_kwargs=_fake_codex_auth)
    assert client._model == CODEX_DEFAULT_MODEL


def test_model_loaded_from_config_when_present(
    _fake_codex_auth: dict[str, Path],
) -> None:
    _fake_codex_auth["config_path"].write_text(
        'model = "gpt-5-custom"\n', encoding="utf-8"
    )
    client = CodexResponsesClient(db_kwargs=_fake_codex_auth)
    assert client._model == "gpt-5-custom"
