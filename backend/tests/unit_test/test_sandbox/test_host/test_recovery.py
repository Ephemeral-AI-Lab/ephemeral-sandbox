"""Tests for sandbox recovery setup routing."""

from __future__ import annotations

import threading
from unittest.mock import MagicMock

import pytest

from sandbox.contracts import RawExecResult


@pytest.fixture(autouse=True)
def _isolate_registry(monkeypatch: pytest.MonkeyPatch) -> None:
    from sandbox.provider import registry as reg

    monkeypatch.setattr(reg, "_ADAPTERS", {}, raising=False)
    monkeypatch.setattr(reg, "_DEFAULT", None, raising=False)
    monkeypatch.setattr(reg, "_LOCK", threading.Lock(), raising=False)


def test_recovery_probe_success_skips_restart_setup(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    from sandbox.host import recovery
    from sandbox.provider.registry import register_adapter

    adapter = MagicMock()
    adapter.get.return_value = {"id": "sb-1", "project_dir": "/testbed"}
    register_adapter("sb-1", adapter)

    async def probe_ok(*_args, **_kwargs) -> RawExecResult:
        return RawExecResult(exit_code=0, stdout="/testbed\n")

    setup_calls: list[tuple[str, str | None]] = []
    adapter.exec = probe_ok
    monkeypatch.setattr(
        recovery,
        "setup_after_start",
        lambda sid, ws: setup_calls.append((sid, ws)),
    )

    assert recovery.ensure_running("sb-1") == {"id": "sb-1", "project_dir": "/testbed"}
    adapter.start.assert_not_called()
    assert setup_calls == []


def test_recovery_restart_uses_canonical_setup_hook(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    from sandbox.host import recovery
    from sandbox.provider.registry import register_adapter

    adapter = MagicMock()
    adapter.get.return_value = {"id": "sb-1", "project_dir": "/testbed"}
    adapter.start.return_value = {"id": "sb-1", "project_dir": "/testbed"}
    register_adapter("sb-1", adapter)

    async def probe_failed(*_args, **_kwargs) -> RawExecResult:
        return RawExecResult(exit_code=1, stdout="")

    setup_calls: list[tuple[str, str | None]] = []
    adapter.exec = probe_failed
    monkeypatch.setattr(
        recovery,
        "setup_after_start",
        lambda sid, ws: setup_calls.append((sid, ws)),
    )

    assert recovery.ensure_running("sb-1") == {"id": "sb-1", "project_dir": "/testbed"}
    adapter.start.assert_called_once_with("sb-1")
    assert setup_calls == [("sb-1", "/testbed")]
