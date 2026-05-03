"""Unit tests for the transport-backed runtime command client."""

from __future__ import annotations

import json
import shlex
from typing import Any

import pytest

from sandbox.runtime.backends import DaemonBackend
from sandbox.runtime.command_client import RuntimeCommandError
from sandbox.runtime.bundle import bundle_hash
from sandbox.providers.registry import dispose_adapter, register_adapter


class _FakeTransport:
    name = "fake"

    def __init__(
        self,
        *,
        response_error: dict[str, Any] | None = None,
        bad_response: bool = False,
    ) -> None:
        self.exec_calls: list[str] = []
        self.response_error = response_error
        self.bad_response = bad_response

    async def exec(
        self,
        sandbox_id: str,
        command: str,
        *,
        cwd: str | None = None,
        timeout: int | None = None,
    ) -> Any:
        del sandbox_id, cwd, timeout
        self.exec_calls.append(command)
        if ".bundle-hash" in command and "tar -xzf" not in command:
            return _result(0, bundle_hash() + "\n")
        if "sandbox.runtime.server" in command:
            if self.bad_response:
                return _result(0, "not-json")
            if self.response_error is not None:
                response = {
                    "success": False,
                    "warnings": [],
                    "timings": {},
                    "error": self.response_error,
                }
            else:
                response = {"success": True, "pong": True, "op": _extract_op(command)}
            return _result(0, json.dumps(response, separators=(",", ":")))
        return _result(0, "")


def _result(exit_code: int, stdout: str) -> Any:
    return type("R", (), {"exit_code": exit_code, "stdout": stdout})()


def _extract_op(command: str) -> str:
    argv = shlex.split(command)
    assert argv[:3] == ["python3", "-m", "sandbox.runtime.server"]
    payload = json.loads(argv[-1])
    return str(payload["op"])


@pytest.mark.asyncio
async def test_call_returns_success_result() -> None:
    transport = _FakeTransport()
    register_adapter("sb-1", transport)
    backend = DaemonBackend(sandbox_id="sb-1", workspace_root="/ws")

    try:
        assert await backend._call_runtime_command("ping") == {
            "success": True,
            "pong": True,
            "op": "ping",
        }
    finally:
        dispose_adapter("sb-1")
    assert any("python3 -m sandbox.runtime.server" in c for c in transport.exec_calls)


@pytest.mark.asyncio
async def test_error_envelope_raises_typed_runtime_command_error() -> None:
    transport = _FakeTransport(
        response_error={
            "kind": "UnsupportedOp",
            "message": "unknown op: nope",
            "details": {"op": "nope"},
        }
    )
    register_adapter("sb-1", transport)
    backend = DaemonBackend(sandbox_id="sb-1", workspace_root="/ws")

    try:
        with pytest.raises(RuntimeCommandError) as exc:
            await backend._call_runtime_command("nope")
    finally:
        dispose_adapter("sb-1")
    assert exc.value.kind == "UnsupportedOp"
    assert exc.value.details == {"op": "nope"}


@pytest.mark.asyncio
async def test_invalid_runtime_server_response_raises_typed_error() -> None:
    transport = _FakeTransport(bad_response=True)
    register_adapter("sb-1", transport)
    backend = DaemonBackend(sandbox_id="sb-1", workspace_root="/ws")

    try:
        with pytest.raises(RuntimeCommandError) as exc:
            await backend._call_runtime_command("ping")
    finally:
        dispose_adapter("sb-1")
    assert exc.value.kind == "BadRuntimeResponse"
