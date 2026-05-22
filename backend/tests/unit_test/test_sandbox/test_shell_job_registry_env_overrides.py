"""Env-var overrides for :func:`get_shell_job_registry` singleton construction.

Phase 2 plan §Step 4: T4 (engine-kill TTL reaper) needs a sub-5-min TTL
window in CI; the production default of 300 s is unusable. These tests
guard the env-var override contract so an operator-time misconfiguration
falls back to safe defaults rather than crashing the daemon.
"""

from __future__ import annotations

import pytest

from sandbox.daemon.service import shell_job as shell_job_module
from sandbox.daemon.service.shell_job import (
    DEFAULT_REAPER_INTERVAL_S,
    DEFAULT_TTL_SECONDS,
    get_shell_job_registry,
    reset_shell_job_registry,
)


@pytest.fixture(autouse=True)
def _reset_singleton() -> None:
    """Drop the singleton before each test so env-var changes take effect."""
    reset_shell_job_registry()
    yield
    reset_shell_job_registry()


def test_defaults_when_env_unset(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv("EOS_SHELL_JOB_TTL_S", raising=False)
    monkeypatch.delenv("EOS_SHELL_JOB_REAPER_INTERVAL_S", raising=False)
    registry = get_shell_job_registry()
    assert registry._ttl_seconds == DEFAULT_TTL_SECONDS
    assert registry._reaper_interval_s == DEFAULT_REAPER_INTERVAL_S


def test_overrides_honored_when_env_set(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("EOS_SHELL_JOB_TTL_S", "10")
    monkeypatch.setenv("EOS_SHELL_JOB_REAPER_INTERVAL_S", "2")
    registry = get_shell_job_registry()
    assert registry._ttl_seconds == 10.0
    assert registry._reaper_interval_s == 2.0


def test_malformed_env_value_falls_back_to_default(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("EOS_SHELL_JOB_TTL_S", "not-a-number")
    monkeypatch.setenv("EOS_SHELL_JOB_REAPER_INTERVAL_S", "")
    registry = get_shell_job_registry()
    assert registry._ttl_seconds == DEFAULT_TTL_SECONDS
    assert registry._reaper_interval_s == DEFAULT_REAPER_INTERVAL_S


def test_negative_env_value_falls_back_to_default(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """A negative TTL would disable the reaper; mask it as a misconfig."""
    monkeypatch.setenv("EOS_SHELL_JOB_TTL_S", "-1")
    monkeypatch.setenv("EOS_SHELL_JOB_REAPER_INTERVAL_S", "-30")
    registry = get_shell_job_registry()
    assert registry._ttl_seconds == DEFAULT_TTL_SECONDS
    assert registry._reaper_interval_s == DEFAULT_REAPER_INTERVAL_S


def test_env_float_helper_directly(monkeypatch: pytest.MonkeyPatch) -> None:
    """Sanity-check the helper without spinning up a registry."""
    monkeypatch.setenv("EOS_TEST_KEY", "42.5")
    assert shell_job_module._env_float_or_default("EOS_TEST_KEY", 1.0) == 42.5
    monkeypatch.setenv("EOS_TEST_KEY", "0")
    assert shell_job_module._env_float_or_default("EOS_TEST_KEY", 1.0) == 0.0
    monkeypatch.delenv("EOS_TEST_KEY")
    assert shell_job_module._env_float_or_default("EOS_TEST_KEY", 7.5) == 7.5
