"""Regression test — ``get_active_model_kwargs`` threads ``class_path``.

Pre-fix bug: ``get_active_model_kwargs`` returned only the JSON ``kwargs``
column and stripped the ``class_path`` row field. Downstream call sites
(`engine.py:117` coding_plan_mode detection and `provider.py:39`
class_path dispatch) both expected ``class_path`` to be in the returned
dict, so plan-mode never dispatched in production — the empty-class_path
fallback always fired and hit the api_key branch.

This test pins the fix: a row registered with class_path = "X" surfaces
``class_path == "X"`` in the dict returned by ``get_active_model_kwargs``.
"""

from __future__ import annotations

from unittest.mock import MagicMock

import pytest

from config.model_config import get_active_model_kwargs, NoActiveModelError


@pytest.fixture
def _patched_store(monkeypatch: pytest.MonkeyPatch) -> MagicMock:
    """Patch ``_resolve_store`` to return a controllable mock."""
    store = MagicMock()
    store.is_ready = True
    monkeypatch.setattr("config.model_config._resolve_store", lambda: store)
    return store


def test_class_path_threaded_into_returned_kwargs(_patched_store: MagicMock) -> None:
    _patched_store.get_active_resolved.return_value = {
        "id": 42,
        "key": "test/plan-mode",
        "label": "Test Plan-Mode",
        "class_path": "providers.clients.coding_plan.anthropic:AnthropicPlanClient",
        "kwargs": {"model": "claude-sonnet-4-5"},
        "is_active": True,
    }

    out = get_active_model_kwargs()

    assert out["class_path"] == (
        "providers.clients.coding_plan.anthropic:AnthropicPlanClient"
    ), "class_path must survive the kwargs extraction for downstream dispatch"
    assert out["model"] == "claude-sonnet-4-5"


def test_empty_class_path_not_injected(_patched_store: MagicMock) -> None:
    """Rows without a class_path don't get a stray empty key."""
    _patched_store.get_active_resolved.return_value = {
        "class_path": "",
        "kwargs": {"api_key": "sk-x", "base_url": "https://example"},
    }

    out = get_active_model_kwargs()

    assert "class_path" not in out
    assert out["api_key"] == "sk-x"


def test_legacy_dot_format_class_path_threaded(_patched_store: MagicMock) -> None:
    """Operator-seeded rows like ``minimax`` use dot-format class_path
    (no colon). The dot-format string must still propagate so
    ``provider.make_api_client`` can decide whether to dispatch via
    importlib (colon present) or fall through to the api_key path
    (no colon)."""
    _patched_store.get_active_resolved.return_value = {
        "class_path": "providers.clients.anthropic_native.AnthropicClient",
        "kwargs": {
            "api_key": "sk-x",
            "base_url": "https://example",
            "model": "MiniMax-M2.7",
        },
    }

    out = get_active_model_kwargs()

    assert out["class_path"] == (
        "providers.clients.anthropic_native.AnthropicClient"
    )
    assert out["api_key"] == "sk-x"


def test_no_active_row_raises(_patched_store: MagicMock) -> None:
    _patched_store.get_active_resolved.return_value = None

    with pytest.raises(NoActiveModelError):
        get_active_model_kwargs()
