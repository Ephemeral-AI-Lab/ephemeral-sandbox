"""Unit tests for the runtime command backend process.exec path."""

from __future__ import annotations

import base64
import json
from typing import Any

import pytest

from sandbox.runtime.backends import DaemonBackend
from sandbox.runtime.command_client import RuntimeCommandError
from sandbox.runtime.bundle import bundle_hash


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
                return _result(0, "not-base64")
            if self.response_error is not None:
                response = {
                    "success": False,
                    "warnings": [],
                    "timings": {},
                    "error": self.response_error,
                }
            else:
                response = {"success": True, "pong": True, "op": _extract_op(command)}
            raw = json.dumps(response, separators=(",", ":")).encode("utf-8")
            return _result(0, base64.b64encode(raw).decode("ascii"))
        return _result(0, "")


def _result(exit_code: int, stdout: str) -> Any:
    return type("R", (), {"exit_code": exit_code, "stdout": stdout})()


def _extract_op(command: str) -> str:
    marker = "base64.b64decode('"
    start = command.index(marker) + len(marker)
    end = command.index("'", start)
    payload = json.loads(base64.b64decode(command[start:end]).decode("utf-8"))
    return str(payload["op"])


@pytest.mark.asyncio
async def test_call_returns_success_result() -> None:
    transport = _FakeTransport()
    backend = DaemonBackend(sandbox_id="sb-1", workspace_root="/ws", transport=transport)  # type: ignore[arg-type]

    assert await backend._call_runtime_command("ping") == {
        "success": True,
        "pong": True,
        "op": "ping",
    }
    assert any("python3 -" in c for c in transport.exec_calls)


@pytest.mark.asyncio
async def test_error_envelope_raises_typed_daemon_command_error() -> None:
    transport = _FakeTransport(
        response_error={
            "kind": "UnsupportedOp",
            "message": "unknown op: nope",
            "details": {"op": "nope"},
        }
    )
    backend = DaemonBackend(sandbox_id="sb-1", workspace_root="/ws", transport=transport)  # type: ignore[arg-type]

    with pytest.raises(RuntimeCommandError) as exc:
        await backend._call_runtime_command("nope")
    assert exc.value.kind == "UnsupportedOp"
    assert exc.value.details == {"op": "nope"}


@pytest.mark.asyncio
async def test_invalid_process_exec_response_raises_unavailable() -> None:
    transport = _FakeTransport(bad_response=True)
    backend = DaemonBackend(sandbox_id="sb-1", workspace_root="/ws", transport=transport)  # type: ignore[arg-type]

    with pytest.raises(Exception, match="runtime dispatcher unreachable after retry"):
        await backend._call_runtime_command("ping")
