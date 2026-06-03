"""Tests for host-side sandbox runtime bundle upload."""

from __future__ import annotations

from types import SimpleNamespace
from typing import Any

from sandbox._shared.models import RawExecResult
import sandbox.host.runtime_bundle as bundle_module


async def test_ensure_runtime_uploaded_uses_raw_exec(monkeypatch) -> None:
    calls: list[tuple[str, str, int | None]] = []

    async def fake_raw_exec(
        sandbox_id: str,
        command: str,
        *,
        cwd: str | None = None,
        timeout: int | None = None,
    ) -> RawExecResult:
        del cwd
        calls.append((sandbox_id, command, timeout))
        if len(calls) == 1:
            return RawExecResult(exit_code=1, stdout="")
        return RawExecResult(exit_code=0, stdout="")

    monkeypatch.setattr(
        bundle_module,
        "get_adapter",
        lambda _sandbox_id: SimpleNamespace(exec=fake_raw_exec),
    )

    digest = await bundle_module.ensure_runtime_uploaded("sb-1")

    assert digest == bundle_module.bundle_hash()
    assert len(calls) >= 4
    assert calls[0][0] == "sb-1"
    assert ".bundle-hash" in calls[0][1]
    assert any("base64 -d" in command for _, command, _ in calls)
    assert "tar -xzf" in calls[-1][1]


async def test_ensure_runtime_uploaded_noops_when_hash_matches(monkeypatch) -> None:
    calls: list[tuple[str, str, int | None]] = []
    digest = bundle_module.bundle_hash()

    async def fake_raw_exec(
        sandbox_id: str,
        command: str,
        **kwargs: Any,
    ) -> RawExecResult:
        calls.append((sandbox_id, command, kwargs.get("timeout")))
        return RawExecResult(exit_code=0, stdout=f"{digest}\n")

    monkeypatch.setattr(
        bundle_module,
        "get_adapter",
        lambda _sandbox_id: SimpleNamespace(exec=fake_raw_exec),
    )

    assert await bundle_module.ensure_runtime_uploaded("sb-1") == digest
    assert len(calls) == 1
    assert ".bundle-hash" in calls[0][1]
