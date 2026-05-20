"""Plan §A5 dispatch contract + §A12 kill switch."""

from __future__ import annotations

import pytest

from config.model_config import NoActiveModelError
from providers.provider import make_api_client


def test_empty_class_path_uses_default_anthropic_client():
    from providers.clients.anthropic_native import AnthropicClient

    client = make_api_client(
        db_kwargs={"api_key": "sk-x", "class_path": "", "base_url": None}
    )
    assert isinstance(client, AnthropicClient)


def test_missing_class_path_uses_default_anthropic_client():
    from providers.clients.anthropic_native import AnthropicClient

    client = make_api_client(db_kwargs={"api_key": "sk-x", "base_url": None})
    assert isinstance(client, AnthropicClient)


def test_malformed_class_path_no_colon_raises():
    with pytest.raises(NoActiveModelError, match="expected 'module.path:ClassName'"):
        make_api_client(db_kwargs={"class_path": "no_colon_here"})


def test_unimportable_module_raises():
    with pytest.raises(NoActiveModelError, match="cannot import module"):
        make_api_client(
            db_kwargs={"class_path": "providers.clients.does_not_exist:Foo"}
        )


def test_attribute_not_found_raises():
    with pytest.raises(NoActiveModelError, match="not found"):
        make_api_client(
            db_kwargs={
                "class_path": "providers.clients.anthropic_native:NotAClass"
            }
        )


def test_attribute_not_a_class_raises():
    with pytest.raises(NoActiveModelError, match="not a class"):
        make_api_client(
            db_kwargs={
                "class_path": "providers.clients.anthropic_native:MAX_RETRIES"
            }
        )


def test_kill_switch_rejects_coding_plan(monkeypatch):
    monkeypatch.setenv("EOS_DISABLE_CODING_PLAN_MODE", "1")
    with pytest.raises(NoActiveModelError, match="Coding plan mode disabled"):
        make_api_client(
            db_kwargs={
                "class_path": "providers.clients.coding_plan.anthropic:AnthropicPlanClient"
            }
        )


def test_kill_switch_does_not_block_non_plan_class_paths(monkeypatch):
    """Even with the kill switch set, non-coding_plan class_paths still go
    through the importlib resolver and surface their own errors."""
    monkeypatch.setenv("EOS_DISABLE_CODING_PLAN_MODE", "1")
    with pytest.raises(NoActiveModelError, match="not a class"):
        make_api_client(
            db_kwargs={
                "class_path": "providers.clients.anthropic_native:MAX_RETRIES"
            }
        )
