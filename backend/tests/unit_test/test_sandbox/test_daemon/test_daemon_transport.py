"""Daemon transport tests for ``_call_daemon``."""

from __future__ import annotations

import json
from types import SimpleNamespace
from typing import Any

import pytest

from sandbox.host import daemon_client as command


def _ok_response() -> str:
    return json.dumps({"success": True, "timings": {}})


def _ready_response(*, ready: bool = True) -> str:
    return json.dumps(
        {
            "success": True,
            "ready": ready,
            "probes": [],
            "timings": {},
        }
    )


def _bootstrap_ready_response() -> str:
    return json.dumps(
        {
            "success": True,
            "ready": False,
            "probes": [
                {
                    "name": "control_plane",
                    "status": "down",
                    "details": {
                        "error_type": "WorkspaceBindingError",
                        "error": "workspace binding is missing",
                    },
                },
                {"name": "data_plane", "status": "ok", "details": {}},
                {"name": "mutation_gate", "status": "ok", "details": {}},
            ],
            "timings": {},
        }
    )


def _assert_rust_client(command_str: str) -> None:
    assert "eosd daemon --client" in command_str
    assert "runtime.sock" in command_str


def _assert_rust_spawn(command_str: str) -> None:
    assert "eosd daemon --spawn" in command_str
    assert "--socket /eos/daemon/runtime.sock" in command_str


async def test_daemon_uses_daemon_thin_client_by_default() -> None:
    seen: list[str] = []

    async def fake_exec(_sandbox_id: str, command_str: str, **_: Any) -> Any:
        seen.append(command_str)
        return SimpleNamespace(stdout=_ok_response(), stderr="", exit_code=0)

    response = await command._call_daemon(
        exec_fn=fake_exec,
        sandbox_id="sb-1",
        op="api.read_file",
        args={"path": "a"},
    )

    assert response == {"success": True, "timings": {}}
    assert len(seen) == 1
    _assert_rust_client(seen[0])


async def test_daemon_uses_tcp_endpoint_before_thin_client(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    seen_tcp: list[tuple[command._DaemonTcpEndpoint, str, int | None]] = []

    async def fake_tcp(
        endpoint: command._DaemonTcpEndpoint,
        payload: str,
        *,
        timeout: int | None,
    ) -> Any:
        seen_tcp.append((endpoint, payload, timeout))
        return SimpleNamespace(stdout=_ok_response(), stderr="", exit_code=0)

    async def fake_exec(_sandbox_id: str, _command_str: str, **_: Any) -> Any:
        raise AssertionError("thin client should not run when TCP succeeds")

    monkeypatch.setattr(command, "_call_tcp_daemon", fake_tcp)
    endpoint = command._DaemonTcpEndpoint(
        host="127.0.0.1",
        port=53913,
        internal_port=37657,
        auth_token="secret",
    )

    response = await command._call_daemon(
        exec_fn=fake_exec,
        sandbox_id="sb-1",
        op="api.read_file",
        args={"path": "a"},
        timeout=15,
        tcp_endpoint=endpoint,
    )

    assert response == {"success": True, "timings": {}}
    assert len(seen_tcp) == 1
    seen_endpoint, payload, seen_timeout = seen_tcp[0]
    assert seen_endpoint == endpoint
    assert seen_timeout == 15
    envelope = json.loads(payload)
    invocation_id = envelope.pop("invocation_id")
    assert invocation_id
    assert envelope == {
        "op": "api.read_file",
        "args": {"path": "a", "invocation_id": invocation_id},
    }


async def test_daemon_tcp_endpoint_falls_back_to_thin_client_on_connect_failure(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    seen_exec: list[str] = []

    async def fake_tcp(
        _endpoint: command._DaemonTcpEndpoint,
        _payload: str,
        *,
        timeout: int | None,
    ) -> Any:
        return SimpleNamespace(
            stdout="",
            stderr="EOS_DAEMON_CONNECT_FAILED:ConnectionRefusedError",
            exit_code=command._THIN_CLIENT_CONNECT_FAILED,
        )

    async def fake_exec(_sandbox_id: str, command_str: str, **_: Any) -> Any:
        seen_exec.append(command_str)
        return SimpleNamespace(stdout=_ok_response(), stderr="", exit_code=0)

    monkeypatch.setattr(command, "_call_tcp_daemon", fake_tcp)
    endpoint = command._DaemonTcpEndpoint(
        host="127.0.0.1",
        port=53913,
        internal_port=37657,
        auth_token="secret",
    )

    response = await command._call_daemon(
        exec_fn=fake_exec,
        sandbox_id="sb-1",
        op="api.read_file",
        args={"path": "a"},
        tcp_endpoint=endpoint,
    )

    assert response == {"success": True, "timings": {}}
    assert len(seen_exec) == 1
    _assert_rust_client(seen_exec[0])


async def test_daemon_tcp_empty_response_is_io_failure(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    async def fake_inner(
        _endpoint: command._DaemonTcpEndpoint,
        _payload: str,
    ) -> str:
        return ""

    monkeypatch.setattr(command, "_call_tcp_daemon_inner", fake_inner)
    endpoint = command._DaemonTcpEndpoint(
        host="127.0.0.1",
        port=53913,
        internal_port=37657,
        auth_token="secret",
    )

    result = await command._call_tcp_daemon(endpoint, "{}", timeout=1)

    assert result.exit_code == command._THIN_CLIENT_IO_FAILED
    assert result.stderr == "EOS_DAEMON_IO_FAILED:empty_response"


async def test_daemon_tcp_empty_response_invalidates_endpoint_cache(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    async def fake_tcp(
        _endpoint: command._DaemonTcpEndpoint,
        _payload: str,
        *,
        timeout: int | None,
    ) -> Any:
        return SimpleNamespace(
            stdout="",
            stderr=command._EMPTY_RESPONSE_MESSAGE,
            exit_code=command._THIN_CLIENT_IO_FAILED,
        )

    async def fake_exec(*_: Any, **__: Any) -> Any:
        raise AssertionError("empty TCP response should be returned to spawn recovery")

    endpoint = command._DaemonTcpEndpoint(
        host="127.0.0.1",
        port=53913,
        internal_port=37657,
        auth_token="secret",
    )
    command._tcp_endpoint_cache["sb-1"] = endpoint
    monkeypatch.setattr(command, "_call_tcp_daemon", fake_tcp)

    result = await command._send_daemon_envelope(
        exec_fn=fake_exec,
        sandbox_id="sb-1",
        envelope_json='{"op":"api.read_file"}',
        cwd="/runtime",
        timeout=15,
        tcp_endpoint=endpoint,
    )

    assert result.exit_code == command._THIN_CLIENT_IO_FAILED
    assert "sb-1" not in command._tcp_endpoint_cache


async def test_daemon_empty_response_retries_lifecycle_op() -> None:
    seen: list[str] = []
    responses: list[Any] = [
        SimpleNamespace(
            stdout="",
            stderr="EOS_DAEMON_IO_FAILED:empty_response",
            exit_code=command._THIN_CLIENT_IO_FAILED,
        ),
        SimpleNamespace(stdout="", stderr="", exit_code=0),
        SimpleNamespace(stdout=_ready_response(), stderr="", exit_code=0),
        SimpleNamespace(stdout=_ok_response(), stderr="", exit_code=0),
    ]

    async def fake_exec(_sandbox_id: str, command_str: str, **_: Any) -> Any:
        seen.append(command_str)
        return responses.pop(0)

    response = await command._call_daemon(
        exec_fn=fake_exec,
        sandbox_id="sb-1",
        op="api.isolated_workspace.enter",
        args={"layer_stack_root": "/tmp/layers", "agent_id": "agent"},
    )

    assert response == {"success": True, "timings": {}}
    assert len(seen) == 4
    _assert_rust_spawn(seen[1])
    assert "api.runtime.ready" in seen[2]


async def test_daemon_empty_response_does_not_replay_command() -> None:
    seen: list[str] = []

    async def fake_exec(_sandbox_id: str, command_str: str, **_: Any) -> Any:
        seen.append(command_str)
        return SimpleNamespace(
            stdout="",
            stderr="EOS_DAEMON_IO_FAILED:empty_response",
            exit_code=command._THIN_CLIENT_IO_FAILED,
        )

    with pytest.raises(command._DaemonDispatchError) as raised:
        await command._call_daemon(
            exec_fn=fake_exec,
            sandbox_id="sb-1",
            op="api.v1.exec_command",
            args={
                "layer_stack_root": "/tmp/layers",
                "agent_id": "agent",
                "cmd": "echo unsafe",
            },
        )

    assert raised.value.kind == "RuntimeExecFailed"
    assert len(seen) == 1


def test_daemon_commands_do_not_forward_host_env(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("UNSUPPORTED_RUNTIME_ENV", "ignored")
    monkeypatch.setenv("EOS_OCC_AUTO_SQUASH_MAX_DEPTH", "64")
    monkeypatch.setenv("EOS_OCC_SQUASH_MODE", "async")

    thin_client = command._daemon_thin_client_command("{}")
    daemon_spawn = command._daemon_spawn_command()

    assert thin_client.startswith("/eos/daemon/eosd daemon --client ")
    assert daemon_spawn.startswith(
        "if [ -r /etc/environment ]; then set -a; . /etc/environment; set +a; fi;"
    )
    _assert_rust_spawn(daemon_spawn)
    assert "UNSUPPORTED_RUNTIME_ENV" not in thin_client
    assert "UNSUPPORTED_RUNTIME_ENV" not in daemon_spawn
    assert "EOS_OCC_SQUASH_MODE" not in daemon_spawn
    assert "EOS_OCC_AUTO_SQUASH_MAX_DEPTH" not in daemon_spawn


def test_daemon_spawn_tracks_runtime_bundle_signature(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(command, "bundle_hash", lambda: "sha-current")

    daemon_spawn = command._daemon_spawn_command()

    assert "runtime.env" in daemon_spawn
    assert "runtime_bundle_sha=sha-current" in daemon_spawn
    _assert_rust_spawn(daemon_spawn)


def test_daemon_spawn_signature_tracks_tcp_port(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(command, "bundle_hash", lambda: "sha-current")
    endpoint = command._DaemonTcpEndpoint(
        host="127.0.0.1",
        port=53913,
        internal_port=37657,
        auth_token="secret",
    )

    daemon_spawn = command._daemon_spawn_command(tcp_endpoint=endpoint)

    assert "runtime_bundle_sha=sha-current;daemon_tcp_port=37657" in daemon_spawn


async def test_ensure_daemon_current_runs_spawn_command(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    seen: list[str] = []
    timeouts: list[int | None] = []

    class Adapter:
        async def exec(self, _sandbox_id: str, command_str: str, **kwargs: Any) -> Any:
            seen.append(command_str)
            timeouts.append(kwargs.get("timeout"))
            return SimpleNamespace(stdout="", stderr="", exit_code=0)

    monkeypatch.setattr(command, "get_adapter", lambda _sandbox_id: Adapter())

    await command.ensure_daemon_current("sb-1")

    assert len(seen) == 1
    _assert_rust_spawn(seen[0])
    assert timeouts == [command._DAEMON_SPAWN_TIMEOUT]


async def test_daemon_transport_spawns_on_socket_missing() -> None:
    seen: list[str] = []
    timeouts: list[int | None] = []
    responses: list[Any] = [
        SimpleNamespace(
            stdout="",
            stderr="EOS_DAEMON_CONNECT_FAILED:ConnectionRefusedError",
            exit_code=command._THIN_CLIENT_CONNECT_FAILED,
        ),
        SimpleNamespace(stdout="", stderr="", exit_code=0),
        SimpleNamespace(stdout=_ready_response(), stderr="", exit_code=0),
        SimpleNamespace(stdout=_ok_response(), stderr="", exit_code=0),
    ]

    async def fake_exec(_sandbox_id: str, command_str: str, **kwargs: Any) -> Any:
        seen.append(command_str)
        timeouts.append(kwargs.get("timeout"))
        return responses.pop(0)

    response = await command._call_daemon(
        exec_fn=fake_exec,
        sandbox_id="sb-1",
        op="api.read_file",
        args={"layer_stack_root": "/tmp/layers", "path": "a"},
    )

    assert response == {"success": True, "timings": {}}
    assert len(seen) == 4
    _assert_rust_client(seen[0])
    _assert_rust_spawn(seen[1])
    assert "api.runtime.ready" in seen[2]
    _assert_rust_client(seen[3])
    assert timeouts[1] == command._DAEMON_SPAWN_TIMEOUT


async def test_daemon_transport_allows_unbound_readiness_for_workspace_bootstrap() -> None:
    seen: list[str] = []
    responses: list[Any] = [
        SimpleNamespace(
            stdout="",
            stderr="EOS_DAEMON_CONNECT_FAILED:ConnectionRefusedError",
            exit_code=command._THIN_CLIENT_CONNECT_FAILED,
        ),
        SimpleNamespace(stdout="", stderr="", exit_code=0),
        SimpleNamespace(stdout=_bootstrap_ready_response(), stderr="", exit_code=0),
        SimpleNamespace(stdout=_ok_response(), stderr="", exit_code=0),
    ]

    async def fake_exec(_sandbox_id: str, command_str: str, **_: Any) -> Any:
        seen.append(command_str)
        return responses.pop(0)

    response = await command._call_daemon(
        exec_fn=fake_exec,
        sandbox_id="sb-1",
        op="api.ensure_workspace_base",
        args={"layer_stack_root": "/tmp/layers", "workspace_root": "/testbed"},
    )

    assert response == {"success": True, "timings": {}}
    assert len(seen) == 4
    assert "api.runtime.ready" in seen[2]
    assert "api.ensure_workspace_base" in seen[3]


async def test_daemon_transport_readiness_failure_fails_closed() -> None:
    responses: list[Any] = [
        SimpleNamespace(
            stdout="",
            stderr="EOS_DAEMON_CONNECT_FAILED:FileNotFoundError",
            exit_code=command._THIN_CLIENT_CONNECT_FAILED,
        ),
        SimpleNamespace(stdout="", stderr="", exit_code=0),
        SimpleNamespace(stdout=_ready_response(ready=False), stderr="", exit_code=0),
    ]

    async def fake_exec(_sandbox_id: str, _command_str: str, **_: Any) -> Any:
        return responses.pop(0)

    with pytest.raises(command._DaemonReadinessError) as exc:
        await command._call_daemon(
            exec_fn=fake_exec,
            sandbox_id="sb-1",
            op="api.read_file",
            args={"layer_stack_root": "/tmp/layers", "path": "a"},
        )

    assert exc.value.kind == "RuntimeNotReady"
    assert exc.value.details["original_op"] == "api.read_file"


async def test_daemon_transport_bad_readiness_response_uses_readiness_error() -> None:
    responses: list[Any] = [
        SimpleNamespace(
            stdout="",
            stderr="EOS_DAEMON_CONNECT_FAILED:FileNotFoundError",
            exit_code=command._THIN_CLIENT_CONNECT_FAILED,
        ),
        SimpleNamespace(stdout="", stderr="", exit_code=0),
        SimpleNamespace(stdout="", stderr="", exit_code=0),
    ]

    async def fake_exec(_sandbox_id: str, _command_str: str, **_: Any) -> Any:
        return responses.pop(0)

    with pytest.raises(command._DaemonReadinessError) as exc:
        await command._call_daemon(
            exec_fn=fake_exec,
            sandbox_id="sb-1",
            op="api.read_file",
            args={"layer_stack_root": "/tmp/layers", "path": "a"},
        )

    assert exc.value.kind == "BadRuntimeReadinessResponse"
    assert exc.value.details["original_op"] == "api.read_file"


async def test_daemon_spawn_failure_fails_closed() -> None:
    seen: list[str] = []
    responses: list[Any] = [
        SimpleNamespace(
            stdout="",
            stderr="EOS_DAEMON_CONNECT_FAILED:ConnectionRefusedError",
            exit_code=command._THIN_CLIENT_CONNECT_FAILED,
        ),
        SimpleNamespace(
            stdout="",
            stderr="sandbox daemon failed to bind socket within 2.5s",
            exit_code=1,
        ),
    ]

    async def fake_exec(_sandbox_id: str, command_str: str, **_: Any) -> Any:
        seen.append(command_str)
        return responses.pop(0)

    with pytest.raises(command._DaemonDispatchError) as exc:
        await command._call_daemon(
            exec_fn=fake_exec,
            sandbox_id="sb-1",
            op="api.read_file",
            args={"path": "a"},
        )

    assert exc.value.kind == "RuntimeExecFailed"
    assert len(seen) == 2
    _assert_rust_client(seen[0])
    _assert_rust_spawn(seen[1])


async def test_daemon_transport_does_not_retry_after_io_failure() -> None:
    async def fake_exec(_sandbox_id: str, _command_str: str, **_: Any) -> Any:
        return SimpleNamespace(
            stdout="",
            stderr="EOS_DAEMON_IO_FAILED:socket.timeout",
            exit_code=command._THIN_CLIENT_IO_FAILED,
        )

    with pytest.raises(command._DaemonDispatchError) as exc:
        await command._call_daemon(
            exec_fn=fake_exec,
            sandbox_id="sb-1",
            op="api.write_file",
            args={"layer_stack_root": "/tmp/layers", "path": "a"},
        )

    assert exc.value.kind == "RuntimeExecFailed"


async def test_call_daemon_envelope_with_connect_retry_retries_transient_connect_failures(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    attempts: list[str] = []
    sleeps: list[float] = []
    responses: list[Any] = [
        SimpleNamespace(
            stdout="",
            stderr="EOS_DAEMON_CONNECT_FAILED:ConnectionRefusedError",
            exit_code=command._THIN_CLIENT_CONNECT_FAILED,
        ),
        SimpleNamespace(
            stdout="",
            stderr="EOS_DAEMON_CONNECT_FAILED:ConnectionRefusedError",
            exit_code=command._THIN_CLIENT_CONNECT_FAILED,
        ),
        SimpleNamespace(stdout=_ok_response(), stderr="", exit_code=0),
    ]

    async def fake_exec(_sandbox_id: str, command_str: str, **_: Any) -> Any:
        attempts.append(command_str)
        return responses.pop(0)

    async def fake_sleep(delay: float) -> None:
        sleeps.append(delay)

    monkeypatch.setattr(command.asyncio, "sleep", fake_sleep)

    result = await command._call_daemon_envelope_with_connect_retry(
        exec_fn=fake_exec,
        sandbox_id="sb-1",
        envelope_json='{"op":"api.read_file"}',
        cwd="/runtime",
        timeout=15,
    )

    assert result.exit_code == 0
    assert len(attempts) == 3
    assert all("eosd daemon --client" in attempt for attempt in attempts)
    assert sleeps == list(command._CONNECT_RETRY_DELAYS_S[:2])


async def test_call_daemon_envelope_with_connect_retry_can_retry_empty_response(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    attempts: list[str] = []
    sleeps: list[float] = []
    responses: list[Any] = [
        SimpleNamespace(
            stdout="",
            stderr=command._EMPTY_RESPONSE_MESSAGE,
            exit_code=command._THIN_CLIENT_IO_FAILED,
        ),
        SimpleNamespace(stdout=_ok_response(), stderr="", exit_code=0),
    ]

    async def fake_exec(_sandbox_id: str, command_str: str, **_: Any) -> Any:
        attempts.append(command_str)
        return responses.pop(0)

    async def fake_sleep(delay: float) -> None:
        sleeps.append(delay)

    monkeypatch.setattr(command.asyncio, "sleep", fake_sleep)

    result = await command._call_daemon_envelope_with_connect_retry(
        exec_fn=fake_exec,
        sandbox_id="sb-1",
        envelope_json='{"op":"api.read_file"}',
        cwd="/runtime",
        timeout=15,
        retry_empty_response=True,
    )

    assert result.exit_code == 0
    assert len(attempts) == 2
    assert sleeps == [command._CONNECT_RETRY_DELAYS_S[0]]


async def test_daemon_transport_rejects_exec_result_without_exit_code() -> None:
    async def fake_exec(_sandbox_id: str, _command_str: str, **_: Any) -> Any:
        return SimpleNamespace(stdout="", stderr="")

    with pytest.raises(command._DaemonDispatchError) as exc:
        await command._call_daemon(
            exec_fn=fake_exec,
            sandbox_id="sb-1",
            op="api.read_file",
            args={"layer_stack_root": "/tmp/layers", "path": "a"},
        )

    assert exc.value.kind == "BadExecResult"
