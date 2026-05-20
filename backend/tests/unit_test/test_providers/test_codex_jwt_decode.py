"""Plan §A15 — JWT extraction of chatgpt_account_id.

Three cases: Auth0-namespaced path, top-level fallback, missing claim raises.
"""

from __future__ import annotations

import base64
import json

import pytest

from providers.clients.coding_plan.codex import (
    CodexCredentialIncompleteError,
    jwt_extract_chatgpt_account_id,
)


def _b64url(data: bytes) -> str:
    return base64.urlsafe_b64encode(data).rstrip(b"=").decode("ascii")


def _make_jwt(payload: dict) -> str:
    header = _b64url(json.dumps({"alg": "RS256", "typ": "JWT"}).encode("utf-8"))
    body = _b64url(json.dumps(payload).encode("utf-8"))
    signature = _b64url(b"signature_unused")
    return f"{header}.{body}.{signature}"


def test_extracts_from_auth0_namespace() -> None:
    token = _make_jwt(
        {
            "sub": "user-1",
            "https://api.openai.com/auth": {"chatgpt_account_id": "acct-namespaced"},
        }
    )
    assert jwt_extract_chatgpt_account_id(token) == "acct-namespaced"


def test_falls_back_to_top_level() -> None:
    token = _make_jwt({"sub": "user-2", "chatgpt_account_id": "acct-top"})
    assert jwt_extract_chatgpt_account_id(token) == "acct-top"


def test_missing_claim_raises() -> None:
    token = _make_jwt({"sub": "user-3"})
    with pytest.raises(CodexCredentialIncompleteError, match="chatgpt_account_id"):
        jwt_extract_chatgpt_account_id(token)


def test_malformed_jwt_segments_raises() -> None:
    with pytest.raises(CodexCredentialIncompleteError, match="3 \\(JWT\\)"):
        jwt_extract_chatgpt_account_id("not.a.valid.jwt")
