"""Tests for shared Daytona client helpers (post-SandboxProxy deletion)."""

from __future__ import annotations

from sandbox.providers.daytona.client.sync import (
    _normalize_dict,
    _normalize_optional_text,
    _timeout_seconds_from_env,
)


class TestNormalizeHelpers:
    def test_normalize_optional_text_strips(self):
        assert _normalize_optional_text("  hello  ") == "hello"

    def test_normalize_optional_text_none_returns_none(self):
        assert _normalize_optional_text(None) is None

    def test_normalize_optional_text_empty_returns_none(self):
        assert _normalize_optional_text("   ") is None

    def test_normalize_dict(self):
        assert _normalize_dict({"  key  ": "  value  "}) == {"key": "value"}

    def test_normalize_dict_skips_empty_keys(self):
        assert _normalize_dict({"  ": "value"}) == {}

    def test_normalize_dict_none_returns_empty(self):
        assert _normalize_dict(None) == {}


class TestTimeoutConfig:
    def test_timeout_defaults_to_long_cold_start_window(self, monkeypatch):
        monkeypatch.delenv("EPHEMERALOS_SANDBOX_TIMEOUT_SECONDS", raising=False)

        assert _timeout_seconds_from_env() == 300.0

    def test_timeout_reads_env_override(self, monkeypatch):
        monkeypatch.setenv("EPHEMERALOS_SANDBOX_TIMEOUT_SECONDS", "420")

        assert _timeout_seconds_from_env() == 420.0

    def test_timeout_invalid_env_uses_default(self, monkeypatch):
        monkeypatch.setenv("EPHEMERALOS_SANDBOX_TIMEOUT_SECONDS", "not-a-number")

        assert _timeout_seconds_from_env() == 300.0
