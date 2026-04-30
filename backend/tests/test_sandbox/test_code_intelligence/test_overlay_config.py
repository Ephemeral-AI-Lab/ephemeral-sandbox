from __future__ import annotations

from sandbox.code_intelligence.overlay import config as overlay_config


def test_overlay_max_concurrent_default_matches_load_concurrency(
    monkeypatch,
) -> None:
    monkeypatch.delenv("EOS_OVERLAY_MAX_CONCURRENT", raising=False)

    assert overlay_config.overlay_max_concurrent() == 20


def test_overlay_max_concurrent_env_override(monkeypatch) -> None:
    monkeypatch.setenv("EOS_OVERLAY_MAX_CONCURRENT", "32")

    assert overlay_config.overlay_max_concurrent() == 32
