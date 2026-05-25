"""Tests for the unguarded ``sandbox.api.raw_exec`` primitive."""

from __future__ import annotations

import pytest

from sandbox.api import RawExecResult
from sandbox.api.raw_exec import raw_exec
from sandbox.provider.registry import dispose_adapter, register_adapter


class RecordingAdapter:
    name = "recording"

    def __init__(self) -> None:
        self.calls: list[tuple[str, str, str | None, int | None]] = []

    async def exec(
        self,
        sandbox_id: str,
        command: str,
        *,
        cwd: str | None = None,
        timeout: int | None = None,
    ) -> RawExecResult:
        self.calls.append((sandbox_id, command, cwd, timeout))
        return RawExecResult(exit_code=7, stdout="out", stderr="err")


async def test_raw_exec_delegates_to_registered_adapter() -> None:
    sandbox_id = "test-raw-exec-delegates"
    adapter = RecordingAdapter()
    dispose_adapter(sandbox_id)
    register_adapter(sandbox_id, adapter)
    try:
        result = await raw_exec(
            sandbox_id,
            "echo hi",
            cwd="/workspace",
            timeout=12,
        )
    finally:
        dispose_adapter(sandbox_id)

    assert result == RawExecResult(exit_code=7, stdout="out", stderr="err")
    assert adapter.calls == [(sandbox_id, "echo hi", "/workspace", 12)]


async def test_raw_exec_unknown_sandbox_id_surfaces_key_error() -> None:
    sandbox_id = "test-raw-exec-missing"
    dispose_adapter(sandbox_id)

    with pytest.raises(KeyError):
        await raw_exec(sandbox_id, "pwd")
