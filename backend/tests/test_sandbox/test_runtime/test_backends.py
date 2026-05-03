"""Unit tests for the CodeIntelligenceBackend Protocol selection + runtime backends.

Covers the four-entry truth table selection logic in
``CodeIntelligenceService._select_backend`` (provider adapter x sandbox_id),
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
from sandbox.providers.registry import dispose_adapter, register_adapter
from sandbox.runtime.registry import (
    dispose_all_code_intelligence,
    get_code_intelligence,
)
from sandbox.runtime.service import CodeIntelligenceService

_REGISTERED_ADAPTERS: list[str] = []


@pytest.fixture(autouse=True)
def _clear_registry() -> None:
    dispose_all_code_intelligence()
    yield
    dispose_all_code_intelligence()
    for sandbox_id in _REGISTERED_ADAPTERS:
        dispose_adapter(sandbox_id)
    _REGISTERED_ADAPTERS.clear()


def _register_adapter(sandbox_id: str) -> None:
    register_adapter(sandbox_id, MagicMock(name=f"adapter-{sandbox_id}"))
    _REGISTERED_ADAPTERS.append(sandbox_id)


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


def test_select_inprocess_without_provider_adapter(tmp_path: Path) -> None:
    """No provider adapter -> InProcess (sandboxless flow)."""
    svc = CodeIntelligenceService(sandbox_id="sb-a", workspace_root=str(tmp_path))
    assert type(svc._impl) is InProcessBackend


def test_select_daemon_with_provider_adapter_and_id(tmp_path: Path) -> None:
    """Provider adapter + sandbox id always selects the daemon backend."""
    _register_adapter("sb-default")
    svc = CodeIntelligenceService(
        sandbox_id="sb-default",
        workspace_root=str(tmp_path),
    )
    assert type(svc._impl) is DaemonBackend


def test_select_daemon_ignores_legacy_env_flag(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    monkeypatch.setenv("EOS_CI_IN_SANDBOX", "0")
    _register_adapter("sb-d")
    svc = CodeIntelligenceService(
        sandbox_id="sb-d",
        workspace_root=str(tmp_path),
    )
    assert type(svc._impl) is DaemonBackend


def test_select_inprocess_with_no_adapter_and_sandbox_id(tmp_path: Path) -> None:
    svc = CodeIntelligenceService(sandbox_id="sb-c", workspace_root=str(tmp_path))
    assert type(svc._impl) is InProcessBackend


def test_select_inprocess_with_empty_sandbox_id(tmp_path: Path) -> None:
    svc = CodeIntelligenceService(
        sandbox_id="",
        workspace_root=str(tmp_path),
    )
    assert type(svc._impl) is InProcessBackend


# ---------------------------------------------------------------------------
# Registry provider-backend matching
# ---------------------------------------------------------------------------


def test_registry_reuses_service_with_same_provider_mode(tmp_path: Path) -> None:
    _register_adapter("registry-reuse")
    first = get_code_intelligence("registry-reuse", str(tmp_path))
    second = get_code_intelligence("registry-reuse", str(tmp_path))
    assert first is second


def test_registry_replaces_service_when_provider_adapter_is_added(
    tmp_path: Path,
) -> None:
    first = get_code_intelligence("registry-replace", str(tmp_path))
    _register_adapter("registry-replace")
    second = get_code_intelligence("registry-replace", str(tmp_path))
    assert second is not first


def test_registry_replaces_service_when_provider_adapter_is_removed(
    tmp_path: Path,
) -> None:
    _register_adapter("registry-remove")
    first = get_code_intelligence("registry-remove", str(tmp_path))
    dispose_adapter("registry-remove")
    second = get_code_intelligence("registry-remove", str(tmp_path))
    assert second is not first


def test_registry_disposes_service_when_provider_adapter_is_removed(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    disposed: list[str] = []
    original_dispose = CodeIntelligenceService.dispose

    def _spy_dispose(self: CodeIntelligenceService) -> None:
        disposed.append(self.sandbox_id)
        original_dispose(self)

    monkeypatch.setattr(CodeIntelligenceService, "dispose", _spy_dispose)

    _register_adapter("registry-dispose")
    first = get_code_intelligence("registry-dispose", str(tmp_path))
    dispose_adapter("registry-dispose")
    second = get_code_intelligence("registry-dispose", str(tmp_path))

    assert second is not first
    assert disposed == ["registry-dispose"]


# ---------------------------------------------------------------------------
# DaemonBackend
# ---------------------------------------------------------------------------


def _build_daemon_backend() -> DaemonBackend:
    return DaemonBackend(
        sandbox_id="sb-daemon",
        workspace_root="/workspace",
    )


def test_daemon_backend_init_attributes() -> None:
    backend = _build_daemon_backend()
    assert backend.sandbox_id == "sb-daemon"
    assert backend.workspace_root == "/workspace"
    assert backend.is_initialized is False


@pytest.mark.asyncio
async def test_daemon_backend_cmd_routes_to_daemon() -> None:
    backend = _build_daemon_backend()
    calls: list[tuple[str, dict[str, object]]] = []

    class _FakeRuntime:
        async def _call_runtime_command(
            self,
            op: str,
            args: dict[str, object],
            *,
            timeout: float = 30.0,
        ) -> dict[str, object]:
            del timeout
            calls.append((op, args))
            return {"result": "hi\n", "exit_code": 0}

    backend._call_runtime_command = _FakeRuntime()._call_runtime_command  # type: ignore[method-assign]
    sandbox = MagicMock()
    result = await backend.cmd(sandbox, "echo hi")

    assert result.result == "hi\n"
    assert result.exit_code == 0
    assert calls == [
        (
            "shell",
            {
                "sandbox_id": "sb-daemon",
                "workspace_root": "/workspace",
                "command": "echo hi",
            },
        )
    ]


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
