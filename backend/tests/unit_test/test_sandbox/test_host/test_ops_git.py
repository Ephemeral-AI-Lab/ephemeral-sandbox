"""Unit tests for sandbox.host.git.ensure_git.

Body lifted from the deleted SandboxProxy.ensure_git, with provider exec
replacing the SDK process.exec. These tests cover the same probe + install
branches that the old TestSandboxProxy class covered.
"""

from __future__ import annotations

from types import SimpleNamespace

import pytest

from sandbox.contract import RawExecResult
from sandbox.host.git import ensure_git


def test_ensure_git_skips_when_git_present(monkeypatch: pytest.MonkeyPatch) -> None:
    calls: list[tuple[str, str, int | None]] = []

    async def fake_raw_exec(sandbox_id, command, *, timeout=None, cwd=None):
        del cwd
        calls.append((sandbox_id, command, timeout))
        return RawExecResult(exit_code=0, stdout="ok")

    monkeypatch.setattr(
        "sandbox.provider.registry.get_adapter",
        lambda _sandbox_id: SimpleNamespace(exec=fake_raw_exec),
    )
    ensure_git("sb-123")

    assert calls == [
        (
            "sb-123",
            "command -v git >/dev/null 2>&1 && echo ok || echo missing",
            10,
        )
    ]


def test_ensure_git_installs_when_missing(monkeypatch: pytest.MonkeyPatch) -> None:
    calls: list[tuple[str, str, int | None]] = []

    async def fake_raw_exec(sandbox_id, command, *, timeout=None, cwd=None):
        del cwd
        calls.append((sandbox_id, command, timeout))
        if len(calls) == 1:
            return RawExecResult(exit_code=0, stdout="missing")
        return RawExecResult(exit_code=0, stdout="installed")

    monkeypatch.setattr(
        "sandbox.provider.registry.get_adapter",
        lambda _sandbox_id: SimpleNamespace(exec=fake_raw_exec),
    )
    ensure_git("sb-123")

    assert len(calls) == 2
    assert calls[1][0] == "sb-123"
    assert calls[1][2] == 120  # the install timeout, not the probe timeout


def test_ensure_git_no_op_for_empty_sandbox_id(monkeypatch: pytest.MonkeyPatch) -> None:
    called = {"value": False}

    async def fake_raw_exec(*_args, **_kwargs):
        called["value"] = True
        return RawExecResult(exit_code=0, stdout="")

    monkeypatch.setattr(
        "sandbox.provider.registry.get_adapter",
        lambda _sandbox_id: SimpleNamespace(exec=fake_raw_exec),
    )
    ensure_git("")

    assert called["value"] is False


def test_ensure_git_swallows_install_failure(monkeypatch: pytest.MonkeyPatch) -> None:
    """A failing install must NOT raise — it's logged best-effort."""

    async def fake_raw_exec(sandbox_id, command, *, timeout=None, cwd=None):
        del sandbox_id, cwd, timeout
        if "command -v git" in command:
            return RawExecResult(exit_code=0, stdout="missing")
        return RawExecResult(exit_code=1, stdout="", stderr="apt failed")

    monkeypatch.setattr(
        "sandbox.provider.registry.get_adapter",
        lambda _sandbox_id: SimpleNamespace(exec=fake_raw_exec),
    )
    ensure_git("sb-123")  # Must not raise.
