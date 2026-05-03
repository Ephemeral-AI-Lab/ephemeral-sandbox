from __future__ import annotations

from sandbox.overlay import config as overlay_config


def test_overlay_max_concurrent_default_matches_load_concurrency(
    monkeypatch,
) -> None:
    monkeypatch.delenv("EOS_OVERLAY_MAX_CONCURRENT", raising=False)

    assert overlay_config.overlay_max_concurrent() == 50


def test_overlay_max_concurrent_env_override(monkeypatch) -> None:
    monkeypatch.setenv("EOS_OVERLAY_MAX_CONCURRENT", "32")

    assert overlay_config.overlay_max_concurrent() == 32
