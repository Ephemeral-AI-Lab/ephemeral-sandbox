"""Tests for the Daytona provider adapter."""

from __future__ import annotations

from types import SimpleNamespace

from sandbox.contract import RawExecResult
from sandbox.provider.daytona.adapter import DaytonaProviderAdapter
from sandbox.provider.daytona.bash import EXIT_MARKER


class _FakeProcess:
    def __init__(self) -> None:
        self.calls: list[tuple[str, int | None]] = []

    async def exec(self, command: str, *, timeout: int | None = None) -> SimpleNamespace:
        self.calls.append((command, timeout))
        return SimpleNamespace(result=f"ok\n{EXIT_MARKER}0\n", exit_code=0)


async def test_daytona_provider_adapter_execs_through_daytona_process() -> None:
    process = _FakeProcess()
    sandbox = SimpleNamespace(process=process)

    async def resolver(sandbox_id: str) -> SimpleNamespace:
        assert sandbox_id == "sb-1"
        return sandbox

    adapter = DaytonaProviderAdapter(sandbox_resolver=resolver)

    result = await adapter.exec(
        "sb-1",
        "echo ok",
        cwd="/workspace",
        timeout=12,
    )

    assert result == RawExecResult(success=True, exit_code=0, stdout="ok", stderr="")
    assert len(process.calls) == 1
    wrapped, timeout = process.calls[0]
    assert timeout == 12
    assert "cd /workspace" in wrapped
    assert "echo ok" in wrapped


async def test_context_registration_installs_daytona_provider_adapter() -> None:
    from sandbox.provider.daytona.context import _register_provider_adapter_if_missing
    from sandbox.provider.registry import dispose_adapter, get_adapter

    sandbox_id = "test-register-provider-adapter"
    dispose_adapter(sandbox_id)

    _register_provider_adapter_if_missing(sandbox_id)

    assert isinstance(get_adapter(sandbox_id), DaytonaProviderAdapter)
    dispose_adapter(sandbox_id)
