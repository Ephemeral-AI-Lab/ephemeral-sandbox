"""Tests for the Daytona sandbox proxy and helpers."""

from __future__ import annotations

from unittest.mock import MagicMock

from sandbox.providers.daytona.client.sync import (
    _normalize_dict,
    _normalize_optional_text,
    _timeout_seconds_from_env,
)
from sandbox.providers.daytona.proxy import SandboxProxy


def _make_proxy(**attrs) -> SandboxProxy:
    """Create a SandboxProxy backed by a MagicMock configured with *attrs*."""
    raw = MagicMock()
    raw.configure_mock(**attrs)
    return SandboxProxy(raw)


class TestSandboxProxy:
    def test_id_returns_raw_id(self):
        assert _make_proxy(id="sb-abc123").id == "sb-abc123"

    def test_name(self):
        assert _make_proxy(name="my-sandbox").name == "my-sandbox"

    def test_created_at(self):
        assert _make_proxy(created_at="2025-01-01T00:00:00Z").created_at == "2025-01-01T00:00:00Z"

    def test_labels_dict(self):
        assert _make_proxy(labels={"key": "value"}).labels == {"key": "value"}

    def test_labels_falls_back_to_empty_dict(self):
        raw = MagicMock(spec=[])
        raw.configure_mock(labels=None)
        assert SandboxProxy(raw).labels == {}

    def test_state_unknown_when_none(self):
        assert _make_proxy(state=None).state == "unknown"

    def test_state_strips_sandboxstate_prefix(self):
        class MockState:
            value = "sandboxstate.started"

        assert _make_proxy(state=MockState()).state == "started"

    def test_state_uses_raw_string(self):
        assert _make_proxy(state="stopped").state == "stopped"

    def test_image_from_snapshot_label(self):
        proxy = _make_proxy(
            labels={"ephemeralos_snapshot": "my-snapshot"},
            image=None, image_name=None, snapshot=None,
        )
        assert proxy.image == "my-snapshot"

    def test_image_from_image_label(self):
        proxy = _make_proxy(
            labels={"ephemeralos_image": "my-image"},
            image=None, image_name=None, snapshot=None,
        )
        assert proxy.image == "my-image"

    def test_managed_by_app_true(self):
        assert _make_proxy(labels={"managed_by": "ephemeralos"}).managed_by_app is True

    def test_managed_by_app_false(self):
        assert _make_proxy(labels={"managed_by": "other"}).managed_by_app is False

    def test_refresh_calls_refresh_data(self):
        refresh_mock = MagicMock()
        proxy = _make_proxy(refresh_data=refresh_mock)
        proxy.refresh()
        refresh_mock.assert_called_once()

    def test_refresh_skips_when_missing(self):
        raw = MagicMock(spec=[])
        SandboxProxy(raw).refresh()  # must not raise

    def test_serialize(self):
        proxy = _make_proxy(
            id="sb-123", name="test-name", created_at="2025-01-01",
            labels={"managed_by": "ephemeralos"}, state="started",
            image=None, image_name=None, snapshot=None,
        )
        result = proxy.serialize(assigned_agents=["agent-1"])
        assert result["id"] == "sb-123"
        assert result["name"] == "test-name"
        assert result["state"] == "started"
        assert result["assigned_agents"] == ["agent-1"]
        assert result["managed_by_app"] is True

    def test_ensure_git_skips_when_git_present(self, monkeypatch):
        from sandbox.api import RawExecResult

        calls: list[tuple[str, str, int | None]] = []

        async def fake_raw_exec(sandbox_id, command, *, timeout=None, cwd=None):
            del cwd
            calls.append((sandbox_id, command, timeout))
            return RawExecResult(exit_code=0, stdout="ok")

        monkeypatch.setattr("sandbox.api.raw_exec.raw_exec", fake_raw_exec)
        proxy = _make_proxy(id="sb-123")
        proxy.ensure_git()
        assert calls == [
            (
                "sb-123",
                "command -v git >/dev/null 2>&1 && echo ok || echo missing",
                10,
            )
        ]

    def test_ensure_git_installs_when_missing(self, monkeypatch):
        from sandbox.api import RawExecResult

        calls: list[tuple[str, str, int | None]] = []

        async def fake_raw_exec(sandbox_id, command, *, timeout=None, cwd=None):
            del cwd
            calls.append((sandbox_id, command, timeout))
            if len(calls) == 1:
                return RawExecResult(exit_code=0, stdout="missing")
            return RawExecResult(exit_code=0, stdout="installed")

        monkeypatch.setattr("sandbox.api.raw_exec.raw_exec", fake_raw_exec)
        proxy = _make_proxy(id="sb-123")
        proxy.ensure_git()
        assert len(calls) == 2
        assert calls[1][0] == "sb-123"
        assert calls[1][2] == 120


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
