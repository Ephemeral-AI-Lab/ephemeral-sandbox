"""Plan §A10 — CLI [coding-plan-mode] <provider> notice at dispatch.

Three cases per S1.2 acceptance criteria. Notice fires per agent spawn
(every ``make_api_client`` call), not once per process.
"""

from __future__ import annotations

import json
import subprocess
from unittest.mock import patch

import pytest

from providers.provider import make_api_client


FAKE_KEYCHAIN_JSON = json.dumps(
    {
        "claudeAiOauth": {
            "accessToken": "sk-ant-oat01-FAKE",
            "refreshToken": "sk-ant-ort01-FAKE",
            "expiresAt": 9999999999999,
            "subscriptionType": "max",
        }
    }
)


def _fake_security_ok(*args, **kwargs):
    return subprocess.CompletedProcess(
        args=args, returncode=0, stdout=FAKE_KEYCHAIN_JSON, stderr=""
    )


@patch("providers.auth_strategy.subprocess.run", side_effect=_fake_security_ok)
def test_plan_mode_notice_prints_provider_segment(
    _mock: object, capsys: pytest.CaptureFixture[str]
) -> None:
    make_api_client(
        db_kwargs={
            "class_path": "providers.clients.coding_plan.anthropic:AnthropicPlanClient"
        }
    )
    captured = capsys.readouterr()
    assert "[coding-plan-mode] anthropic\n" in captured.out


def test_api_mode_emits_no_notice(capsys: pytest.CaptureFixture[str]) -> None:
    make_api_client(db_kwargs={"api_key": "sk-x", "class_path": "", "base_url": None})
    captured = capsys.readouterr()
    assert "[coding-plan-mode]" not in captured.out


@patch("providers.auth_strategy.subprocess.run", side_effect=_fake_security_ok)
def test_plan_mode_notice_fires_per_spawn(
    _mock: object, capsys: pytest.CaptureFixture[str]
) -> None:
    class_path = "providers.clients.coding_plan.anthropic:AnthropicPlanClient"
    make_api_client(db_kwargs={"class_path": class_path})
    make_api_client(db_kwargs={"class_path": class_path})
    captured = capsys.readouterr()
    assert captured.out.count("[coding-plan-mode] anthropic\n") == 2
