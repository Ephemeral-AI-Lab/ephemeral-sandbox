"""Plan §A8 Part 3 — subprocess env token-leak regression.

Under a plan-mode-active recorder, any subprocess spawned through
``sandbox.execution.subprocess_runner`` must NOT carry vendor OAuth
tokens in its env. Intercept ``subprocess.Popen`` at the import site,
capture the env kwarg, assert the forbidden keys + token-shaped values
are absent.

Two vendor cases (anthropic, codex). Codex's client lands in S4; the
guard runs today against env keys/values regardless because the
property — "no token-shaped env passes through to subprocess children
under a plan-mode run" — is what we want to enforce as a contract.

Note: subprocess_runner uses only ``subprocess.Popen``; no
``asyncio.create_subprocess_exec`` site exists today, so only the one
import-site monkeypatch is applied (verified by grep before writing).
"""

from __future__ import annotations

import json
import re
import subprocess
from pathlib import Path
from typing import Any
from unittest.mock import patch

import pytest


FAKE_ANTHROPIC_KEYCHAIN_JSON = json.dumps(
    {
        "claudeAiOauth": {
            "accessToken": "sk-ant-oat01-FAKE_TOKEN_LITERAL_DO_NOT_LEAK",
            "refreshToken": "sk-ant-ort01-FAKE_TOKEN_LITERAL_DO_NOT_LEAK",
            "expiresAt": 9999999999999,
            "subscriptionType": "max",
        }
    }
)


def _fake_security_ok(*args: Any, **kwargs: Any) -> subprocess.CompletedProcess:
    return subprocess.CompletedProcess(
        args=args, returncode=0, stdout=FAKE_ANTHROPIC_KEYCHAIN_JSON, stderr=""
    )


class _PopenSpy:
    """Minimal spy with the surface area subprocess_to_refs touches."""

    captured_envs: list[dict[str, str]] = []

    def __init__(self, *_args: Any, **kwargs: Any) -> None:
        env = kwargs.get("env") or {}
        _PopenSpy.captured_envs.append(dict(env))

    def wait(self, timeout: float | None = None) -> int:
        return 0

    def poll(self) -> int:
        return 0

    @property
    def pid(self) -> int:
        return -1


def _scrub_token_env(monkeypatch: pytest.MonkeyPatch) -> None:
    """Clear any pre-existing token env vars so the test sees a clean baseline."""
    for key in (
        "CLAUDE_CODE_OAUTH_TOKEN",
        "ANTHROPIC_API_KEY",
        "OPENAI_API_KEY",
    ):
        monkeypatch.delenv(key, raising=False)


def _install_popen_spy(monkeypatch: pytest.MonkeyPatch) -> None:
    _PopenSpy.captured_envs.clear()
    monkeypatch.setattr(
        "sandbox.execution.subprocess_runner.subprocess.Popen", _PopenSpy
    )


def _run_one_subprocess(tmp_path: Path) -> None:
    from sandbox.execution.subprocess_runner import run_command_to_refs

    workspace = tmp_path / "workspace"
    workspace.mkdir(exist_ok=True)
    run_command_to_refs(
        command=["true"],
        declared_workspace_root="/workspace",
        mounted_workspace_root=workspace,
        cwd=".",
        env={},
        timeout_seconds=10.0,
        stdout_ref=tmp_path / "out",
        stderr_ref=tmp_path / "err",
    )


# ---------------------------------------------------------------------------
# Anthropic
# ---------------------------------------------------------------------------

_ANTHROPIC_TOKEN_REGEX = re.compile(r"sk-ant-(oat|ort)01-[A-Za-z0-9_-]+")


def test_no_anthropic_token_in_subprocess_env(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    _scrub_token_env(monkeypatch)
    _install_popen_spy(monkeypatch)

    # Build a plan-mode client; token goes into client memory, NOT env.
    with patch(
        "providers.auth_strategy.subprocess.run", side_effect=_fake_security_ok
    ):
        from providers.clients.coding_plan.anthropic import AnthropicPlanClient

        _ = AnthropicPlanClient()

    _run_one_subprocess(tmp_path)

    assert _PopenSpy.captured_envs, "Popen spy never invoked"
    for env in _PopenSpy.captured_envs:
        assert "CLAUDE_CODE_OAUTH_TOKEN" not in env
        assert "ANTHROPIC_API_KEY" not in env
        for value in env.values():
            assert not _ANTHROPIC_TOKEN_REGEX.match(str(value)), (
                f"Anthropic token-shaped value leaked: {value!r}"
            )


# ---------------------------------------------------------------------------
# Codex
# ---------------------------------------------------------------------------

_CODEX_JWT_REGEX = re.compile(
    r"eyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+"
)

# Sentinel literals reused from the runtime-payload leak test. If the
# Codex client ever sets these in subprocess env, that's the leak we want
# to catch.
_FAKE_CODEX_ACCESS_TOKEN = "codex_access_FAKE_TOKEN_LITERAL_DO_NOT_LEAK"
_FAKE_CODEX_ID_TOKEN = "eyJ.FAKE_CODEX_JWT_PAYLOAD_DO_NOT_LEAK.signature"


def test_no_codex_token_in_subprocess_env(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    _scrub_token_env(monkeypatch)
    _install_popen_spy(monkeypatch)

    _run_one_subprocess(tmp_path)

    assert _PopenSpy.captured_envs, "Popen spy never invoked"
    for env in _PopenSpy.captured_envs:
        assert "OPENAI_API_KEY" not in env
        for value in env.values():
            text = str(value)
            assert text != _FAKE_CODEX_ACCESS_TOKEN, (
                f"Codex access_token literal leaked: {value!r}"
            )
            assert text != _FAKE_CODEX_ID_TOKEN, (
                f"Codex id_token literal leaked: {value!r}"
            )
            assert not _CODEX_JWT_REGEX.match(text), (
                f"JWT-shaped value leaked: {value!r}"
            )
