"""Unit tests for the CodeIntelligenceBackend Protocol selection + in-process/daemon backends.

Covers the four-entry truth table selection logic in
``CodeIntelligenceService._select_backend`` (transport x sandbox_id),
the InProcessBackend behavioral defaults, and that DaemonBackend exposes
the full protocol shape.
"""

from __future__ import annotations

import inspect
from pathlib import Path
from unittest.mock import MagicMock

import pytest

from sandbox.runtime.backends import (
    CodeIntelligenceBackend,
    InProcessBackend,
    DaemonBackend,
)
from sandbox.runtime.registry import (
    dispose_all_code_intelligence,
    get_code_intelligence,
)
from sandbox.runtime.service import CodeIntelligenceService


@pytest.fixture(autouse=True)
def _clear_registry() -> None:
    dispose_all_code_intelligence()
    yield
    dispose_all_code_intelligence()


# ---------------------------------------------------------------------------
# InProcessBackend behavior
# ---------------------------------------------------------------------------


def test_inprocess_is_initialized_starts_false(tmp_path: Path) -> None:
    backend = InProcessBackend(sandbox_id="sb-2", workspace_root=str(tmp_path))
    assert backend.is_initialized is False


def test_inprocess_exposes_required_components(tmp_path: Path) -> None:
    backend = InProcessBackend(sandbox_id="sb-3", workspace_root=str(tmp_path))
    # Load-bearing attributes for mutation callers that read internals.
    assert backend.arbiter is not None
    assert backend.patcher is not None
    assert backend._content is not None
    assert backend._write_coordinator is not None
    assert backend._mutations is not None
    assert backend._command_executor is not None


# ---------------------------------------------------------------------------
# CodeIntelligenceService backend-selection truth table
# ---------------------------------------------------------------------------


def test_select_inprocess_with_no_transport(tmp_path: Path) -> None:
    """No transport at all -> InProcess (sandboxless flow)."""
    svc = CodeIntelligenceService(sandbox_id="sb-a", workspace_root=str(tmp_path))
    assert type(svc._impl) is InProcessBackend


def test_select_daemon_with_transport_and_id(tmp_path: Path) -> None:
    """Transport + sandbox id always selects the daemon backend."""
    transport = MagicMock(name="SandboxTransport")
    svc = CodeIntelligenceService(
        sandbox_id="sb-default",
        workspace_root=str(tmp_path),
        transport=transport,
    )
    assert type(svc._impl) is DaemonBackend


def test_select_daemon_ignores_legacy_env_flag(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    monkeypatch.setenv("EOS_CI_IN_SANDBOX", "0")
    transport = MagicMock(name="SandboxTransport")
    svc = CodeIntelligenceService(
        sandbox_id="sb-d",
        workspace_root=str(tmp_path),
        transport=transport,
    )
    assert type(svc._impl) is DaemonBackend


def test_select_inprocess_with_no_transport_and_sandbox_id(tmp_path: Path) -> None:
    svc = CodeIntelligenceService(sandbox_id="sb-c", workspace_root=str(tmp_path))
    assert type(svc._impl) is InProcessBackend


def test_select_inprocess_with_empty_sandbox_id(tmp_path: Path) -> None:
    transport = MagicMock(name="SandboxTransport")
    svc = CodeIntelligenceService(
        sandbox_id="",
        workspace_root=str(tmp_path),
        transport=transport,
    )
    assert type(svc._impl) is InProcessBackend


# ---------------------------------------------------------------------------
# Registry transport matching
# ---------------------------------------------------------------------------


def test_registry_reuses_service_with_same_transport(tmp_path: Path) -> None:
    transport = MagicMock(name="transport-a")
    first = get_code_intelligence("registry-reuse", str(tmp_path), transport=transport)
    second = get_code_intelligence("registry-reuse", str(tmp_path), transport=transport)
    assert first is second


def test_registry_replaces_service_when_transport_changes(tmp_path: Path) -> None:
    transport_a = MagicMock(name="transport-a")
    transport_b = MagicMock(name="transport-b")
    first = get_code_intelligence("registry-replace", str(tmp_path), transport=transport_a)
    second = get_code_intelligence(
        "registry-replace",
        str(tmp_path),
        transport=transport_b,
    )
    assert second is not first


def test_registry_replaces_service_when_transport_is_removed(tmp_path: Path) -> None:
    transport = MagicMock(name="transport")
    first = get_code_intelligence("registry-remove", str(tmp_path), transport=transport)
    second = get_code_intelligence("registry-remove", str(tmp_path))
    assert second is not first


def test_registry_disposes_service_when_transport_is_removed(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    disposed: list[str] = []
    original_dispose = CodeIntelligenceService.dispose

    def _spy_dispose(self: CodeIntelligenceService) -> None:
        disposed.append(self.sandbox_id)
        original_dispose(self)

    monkeypatch.setattr(CodeIntelligenceService, "dispose", _spy_dispose)

    transport = MagicMock(name="transport")
    first = get_code_intelligence("registry-dispose", str(tmp_path), transport=transport)
    second = get_code_intelligence("registry-dispose", str(tmp_path))

    assert second is not first
    assert disposed == ["registry-dispose"]


# ---------------------------------------------------------------------------
# DaemonBackend
# ---------------------------------------------------------------------------


def _build_daemon_backend() -> DaemonBackend:
    transport = MagicMock(name="SandboxTransport")
    return DaemonBackend(
        sandbox_id="sb-daemon",
        workspace_root="/workspace",
        transport=transport,
    )


def test_daemon_backend_init_attributes() -> None:
    backend = _build_daemon_backend()
    assert backend.sandbox_id == "sb-daemon"
    assert backend.workspace_root == "/workspace"
    assert backend.is_initialized is False
    assert backend._transport is not None


@pytest.mark.asyncio
async def test_daemon_backend_cmd_routes_to_daemon() -> None:
    backend = _build_daemon_backend()
    calls: list[tuple[str, dict[str, object]]] = []

    class _FakeDaemon:
        async def _call_daemon_command(
            self,
            op: str,
            args: dict[str, object],
            *,
            timeout: float = 30.0,
        ) -> dict[str, object]:
            del timeout
            calls.append((op, args))
            return {"result": "hi\n", "exit_code": 0}

    backend._call_daemon_command = _FakeDaemon()._call_daemon_command  # type: ignore[method-assign]
    sandbox = MagicMock()
    result = await backend.cmd(sandbox, "echo hi")

    assert result.result == "hi\n"
    assert result.exit_code == 0
    assert calls == [("svc_cmd", {"command": "echo hi"})]


def test_daemon_backend_rebind_sandbox_is_noop() -> None:
    """Daemon's CodeIntelligenceService is constructed with sandbox=None;
    rebinding from the orchestrator side is a no-op on the daemon backend."""
    backend = _build_daemon_backend()
    backend.rebind_sandbox(MagicMock(name="sandbox"))


# ---------------------------------------------------------------------------
# Protocol shape — sanity check that InProcessBackend implements every CodeIntelligenceBackend op
# ---------------------------------------------------------------------------


def test_inprocess_satisfies_protocol_shape() -> None:
    """Every public method declared on CodeIntelligenceBackend exists on InProcessBackend."""
    declared = {
        name
        for name, value in inspect.getmembers(CodeIntelligenceBackend)
        if not name.startswith("_") and callable(value)
    }
    implemented = {
        name
        for name, value in inspect.getmembers(InProcessBackend)
        if not name.startswith("_") and callable(value)
    }
    missing = declared - implemented
    assert missing == set(), f"InProcessBackend missing methods: {missing}"


def test_daemon_satisfies_protocol_shape() -> None:
    """Every public method declared on CodeIntelligenceBackend exists on DaemonBackend."""
    declared = {
        name
        for name, value in inspect.getmembers(CodeIntelligenceBackend)
        if not name.startswith("_") and callable(value)
    }
    implemented = {
        name
        for name, value in inspect.getmembers(DaemonBackend)
        if not name.startswith("_") and callable(value)
    }
    missing = declared - implemented
    assert missing == set(), f"DaemonBackend missing methods: {missing}"
