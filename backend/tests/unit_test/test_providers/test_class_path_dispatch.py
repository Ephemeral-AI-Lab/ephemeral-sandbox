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


def test_no_colon_class_path_falls_through_to_api_key_dispatch():
    """Legacy operator-seeded rows (e.g., ``minimax`` registry entry) store
    ``class_path`` in dot-format with no colon. Such rows are treated as
    api-mode: dispatch falls through to the api_key path. The actual error
    surface depends on whether ``api_key`` is present — here we assert the
    api-key path fires (raising for the missing key) rather than the
    importlib path firing (which would complain about a missing colon).
    """
    with pytest.raises(NoActiveModelError, match="no api_key"):
        make_api_client(db_kwargs={"class_path": "no_colon_here"})


def test_no_colon_class_path_with_api_key_constructs_default_anthropic_client():
    """No-colon class_path + api_key + base_url present → AnthropicClient."""
    from providers.clients.anthropic_native import AnthropicClient

    client = make_api_client(
        db_kwargs={
            "class_path": "providers.clients.anthropic_native.AnthropicClient",
            "api_key": "sk-x",
            "base_url": None,
        }
    )
    assert isinstance(client, AnthropicClient)


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
