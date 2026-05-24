"""Unit tests for tier0_health.probe_tier0 — covers PRD T-A2 acceptance."""

from __future__ import annotations

import subprocess

import pytest

from tests.live_e2e_test._tools import tier0_health
from tests.live_e2e_test._tools.tier0_health import Tier0Result, probe_tier0


@pytest.fixture(autouse=True)
def _no_real_network(monkeypatch):
    """Hard-fail if a test forgets to stub _check_api_health."""

    monkeypatch.setenv("EOS_SANDBOX_PROVIDER", "daytona")

    def _explode(*_a, **_kw):
        raise RuntimeError("real urllib call leaked into unit test")

    monkeypatch.setattr("urllib.request.urlopen", _explode)
    monkeypatch.setattr(
        tier0_health,
        "_detect_runner_bootstrap_issue",
        lambda timeout_s=5.0: tier0_health.RunnerBootstrapIssue(
            docker_available=False,
            runner_healthy=None,
        ),
    )


def _stub_health(monkeypatch, status: tier0_health.ApiHealth, note: str = "") -> None:
    monkeypatch.setattr(
        tier0_health,
        "_check_api_health",
        lambda url, timeout_s: (status, note),
    )


def _stub_detect(
    monkeypatch,
    *,
    docker_available: bool,
    rows: list[str],
    note: str = "",
) -> None:
    monkeypatch.setattr(
        tier0_health,
        "_detect_stuck_rows",
        lambda timeout_s=5.0: (docker_available, list(rows), note),
    )


def _stub_runner(
    monkeypatch,
    *,
    docker_available: bool = True,
    runner_healthy: bool | None = True,
    stale_pid: str | None = None,
    note: str = "",
) -> None:
    monkeypatch.setattr(
        tier0_health,
        "_detect_runner_bootstrap_issue",
        lambda timeout_s=5.0: tier0_health.RunnerBootstrapIssue(
            docker_available=docker_available,
            runner_healthy=runner_healthy,
            stale_containerd_pid=stale_pid,
            notes=note,
        ),
    )


def test_healthy_api_no_docker_passes(monkeypatch):
    _stub_health(monkeypatch, "ok", "http_code=200")
    _stub_detect(monkeypatch, docker_available=False, rows=[], note="docker_unavailable")
    result = probe_tier0("http://daytona.local/api")
    assert isinstance(result, Tier0Result)
    assert result.passed is True
    assert result.api_health == "ok"
    assert result.docker_available is False
    assert result.recovery_attempted is False
    assert result.stuck_rows == []
    assert "docker_unavailable" in result.notes


def test_healthy_api_docker_no_stuck_passes(monkeypatch):
    _stub_health(monkeypatch, "ok", "http_code=200")
    _stub_detect(monkeypatch, docker_available=True, rows=[])
    _stub_runner(monkeypatch, note="runner_health=healthy")
    result = probe_tier0("http://daytona.local/api")
    assert result.passed is True
    assert result.api_health == "ok"
    assert result.docker_available is True
    assert result.stuck_rows == []
    assert result.runner_healthy is True
    assert result.recovery_attempted is False


def test_healthy_api_unhealthy_runner_with_stale_pid_fails(monkeypatch):
    _stub_health(monkeypatch, "ok", "http_code=200")
    _stub_detect(monkeypatch, docker_available=True, rows=[])
    _stub_runner(
        monkeypatch,
        runner_healthy=False,
        stale_pid="97",
        note="runner_health=unhealthy; stale_containerd_pid=97",
    )
    result = probe_tier0("http://daytona.local/api")
    assert result.passed is False
    assert result.stale_containerd_pid == "97"
    assert result.recovery_attempted is False
    assert "tier0_runner_recovery_required" in result.notes


def test_auto_recover_runner_stale_pid_succeeds(monkeypatch):
    _stub_health(monkeypatch, "ok", "http_code=200")
    _stub_detect(monkeypatch, docker_available=True, rows=[])
    _stub_runner(
        monkeypatch,
        runner_healthy=False,
        stale_pid="97",
        note="runner_health=unhealthy; stale_containerd_pid=97",
    )
    monkeypatch.setattr(
        tier0_health,
        "_recover_runner_bootstrap",
        lambda timeout_s=30.0: (True, "runner_restart_succeeded=true"),
    )
    result = probe_tier0("http://daytona.local/api", auto_recover=True)
    assert result.passed is True
    assert result.recovery_attempted is True
    assert result.recovery_succeeded is True
    assert "runner_restart_succeeded=true" in result.notes


def test_healthy_api_docker_with_stuck_rows_fails(monkeypatch):
    _stub_health(monkeypatch, "ok", "http_code=200")
    _stub_detect(monkeypatch, docker_available=True, rows=["sb-1", "sb-2"])
    result = probe_tier0("http://daytona.local/api")
    assert result.passed is False
    assert result.stuck_rows == ["sb-1", "sb-2"]
    assert result.recovery_attempted is False
    assert "tier0_manual_recovery_required" in result.notes


def test_api_timeout_fails(monkeypatch):
    _stub_health(monkeypatch, "timeout", "socket_timeout")
    _stub_detect(monkeypatch, docker_available=False, rows=[])
    result = probe_tier0("http://daytona.local/api")
    assert result.passed is False
    assert result.api_health == "timeout"


def test_api_non_200_fails(monkeypatch):
    _stub_health(monkeypatch, "non_200", "http_code=503")
    _stub_detect(monkeypatch, docker_available=True, rows=[])
    result = probe_tier0("http://daytona.local/api")
    assert result.passed is False
    assert result.api_health == "non_200"


def test_auto_recover_succeeds_clears_failure(monkeypatch):
    """When docker is up, stuck rows present, auto_recover=True and recovery
    succeeds, the probe should report passed=True."""
    _stub_health(monkeypatch, "ok", "http_code=200")
    _stub_detect(monkeypatch, docker_available=True, rows=["sb-1"])
    monkeypatch.setattr(tier0_health, "_run_recovery", lambda timeout_s=10.0: (True, ""))
    result = probe_tier0("http://daytona.local/api", auto_recover=True)
    assert result.passed is True
    assert result.recovery_attempted is True
    assert result.recovery_succeeded is True


def test_auto_recover_fails_keeps_failure(monkeypatch):
    _stub_health(monkeypatch, "ok", "http_code=200")
    _stub_detect(monkeypatch, docker_available=True, rows=["sb-1"])
    monkeypatch.setattr(
        tier0_health,
        "_run_recovery",
        lambda timeout_s=10.0: (False, "recovery_stderr=permission denied"),
    )
    result = probe_tier0("http://daytona.local/api", auto_recover=True)
    assert result.passed is False
    assert result.recovery_attempted is True
    assert result.recovery_succeeded is False
    assert "permission denied" in result.notes


# --- _detect_stuck_rows internals -----------------------------------------


def test_detect_stuck_rows_no_docker_returns_unavailable(monkeypatch):
    monkeypatch.setattr(tier0_health.shutil, "which", lambda _name: None)
    available, rows, note = tier0_health._detect_stuck_rows()
    assert available is False
    assert rows == []
    assert note == "docker_unavailable"


def test_detect_stuck_rows_parses_psql_output(monkeypatch):
    monkeypatch.setattr(tier0_health.shutil, "which", lambda _name: "/usr/bin/docker")

    fake_completed = subprocess.CompletedProcess(
        args=["docker"],
        returncode=0,
        stdout="abc-1\n\ndef-2\n",
        stderr="",
    )
    captured: dict[str, object] = {}

    def fake_run(*args, **kwargs):
        captured["args"] = args
        captured["kwargs"] = kwargs
        return fake_completed

    monkeypatch.setattr(
        tier0_health.subprocess,
        "run",
        fake_run,
    )

    available, rows, note = tier0_health._detect_stuck_rows()
    assert available is True
    assert rows == ["abc-1", "def-2"]
    assert note == ""
    command = captured["args"][0]
    assert isinstance(command, list)
    assert "state IN ('starting', 'pending_build')" in " ".join(command)


def test_detect_stuck_rows_handles_psql_error(monkeypatch):
    monkeypatch.setattr(tier0_health.shutil, "which", lambda _name: "/usr/bin/docker")
    fake_completed = subprocess.CompletedProcess(
        args=["docker"],
        returncode=1,
        stdout="",
        stderr="container not found",
    )
    monkeypatch.setattr(
        tier0_health.subprocess, "run", lambda *a, **kw: fake_completed
    )
    available, rows, note = tier0_health._detect_stuck_rows()
    assert available is True
    assert rows == []
    assert "container not found" in note
