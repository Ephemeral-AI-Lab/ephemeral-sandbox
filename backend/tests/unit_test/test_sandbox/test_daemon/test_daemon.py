"""Tests for the resident sandbox daemon."""

from __future__ import annotations

import asyncio
import json
import os
import socket
import tempfile
import uuid
from pathlib import Path

import pytest

from sandbox.daemon import builtin_operations as workspace_handler
from sandbox.daemon.rpc import dispatcher as server
from sandbox.daemon.rpc import server as daemon
from sandbox.daemon import layer_stack_runtime, occ_runtime_services


def _short_socket_path() -> tuple[Path, Path]:
    """Return ``(socket, pid)`` paths short enough for AF_UNIX (≤104 bytes)."""
    base = Path(tempfile.gettempdir()) / f"eos-daemon-{uuid.uuid4().hex[:8]}"
    base.mkdir(parents=True, exist_ok=True)
    return base / "runtime.sock", base / "runtime.pid"


def _free_tcp_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.bind(("127.0.0.1", 0))
        return int(sock.getsockname()[1])


@pytest.fixture(autouse=True)
def _isolate_op_table() -> None:
    saved = dict(server.OP_TABLE)
    server.OP_TABLE.clear()
    try:
        yield
    finally:
        server.OP_TABLE.clear()
        server.OP_TABLE.update(saved)


async def test_dispatch_envelope_async_runs_async_handler() -> None:
    async def handler(args: dict[str, object]) -> dict[str, object]:
        await asyncio.sleep(0)
        return {"success": True, "value": args["value"]}

    server.register_op("test.async_echo", handler)

    response = await server.dispatch_envelope_async({"op": "test.async_echo", "args": {"value": 7}})

    assert response["success"] is True
    assert response["value"] == 7
    assert "runtime.boot_to_dispatch_s" in response["timings"]


async def test_dispatch_envelope_async_runs_sync_handler() -> None:
    def handler(args: dict[str, object]) -> dict[str, object]:
        return {"success": True, "value": args["value"] * 2}

    server.register_op("test.sync_echo", handler)

    response = await server.dispatch_envelope_async({"op": "test.sync_echo", "args": {"value": 5}})
    assert response["success"] is True
    assert response["value"] == 10


async def test_dispatch_envelope_async_unknown_op_returns_structured_error() -> None:
    response = await server.dispatch_envelope_async({"op": "nope", "args": {}})
    assert response["success"] is False
    assert response["error"]["kind"] == "unknown_op"


async def test_dispatch_plugin_op_without_agent_id_only_audits_when_unbootstrapped(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    audits: list[dict[str, object]] = []

    async def handler(_: dict[str, object]) -> dict[str, object]:
        return {"success": True}

    class _BootstrappedPipeline:
        @staticmethod
        def get_handle(agent_id: str) -> object | None:
            assert agent_id == ""
            return None

    server.register_op("api.plugin.ensure", handler)
    monkeypatch.setattr(server, "append_jsonl_event", lambda _path, event: audits.append(event))
    monkeypatch.setattr(server, "get_active_pipeline", lambda: None)

    unbootstrapped = await server.dispatch_envelope_async({"op": "api.plugin.ensure", "args": {}})

    assert unbootstrapped["success"] is True
    assert audits == [
        {
            "type": "workspace_lifecycle.plugin_check_unbootstrapped",
            "payload": {"op": "api.plugin.ensure", "agent_id": ""},
        }
    ]

    audits.clear()
    monkeypatch.setattr(server, "get_active_pipeline", lambda: _BootstrappedPipeline())

    bootstrapped = await server.dispatch_envelope_async({"op": "api.plugin.ensure", "args": {}})

    assert bootstrapped["success"] is True
    assert audits == []


async def test_dispatch_envelope_async_honors_boot_t0_override() -> None:
    """``boot_t0`` overrides module-level ``_DISPATCHER_BOOT_MONOTONIC`` so
    daemon-mode dispatch measures per-call boot, not daemon uptime."""
    from sandbox._shared.clock import monotonic_now

    def handler(_: dict[str, object]) -> dict[str, object]:
        return {"success": True}

    server.register_op("test.boot", handler)

    # Pretend the daemon has been running for hours: real `_DISPATCHER_BOOT_MONOTONIC` is far
    # in the past. With the per-call override, we should still see a small
    # boot_to_dispatch.
    response = await server.dispatch_envelope_async(
        {"op": "test.boot", "args": {}},
        boot_t0=monotonic_now(),
    )
    assert response["success"] is True
    assert response["timings"]["runtime.boot_to_dispatch_s"] < 0.05


async def test_daemon_serves_one_envelope_per_connection() -> None:
    socket_path, pid_path = _short_socket_path()

    async def echo(args: dict[str, object]) -> dict[str, object]:
        return {"success": True, "value": args["value"]}

    server.register_op("test.echo", echo)

    serve_task = asyncio.create_task(daemon.serve(socket_path, pid_path))
    try:
        for _ in range(50):
            if socket_path.exists():
                break
            await asyncio.sleep(0.02)
        assert socket_path.exists(), "daemon never bound socket"
        assert pid_path.read_text().strip() == str(os.getpid())

        async def call(value: int) -> dict[str, object]:
            reader, writer = await asyncio.open_unix_connection(str(socket_path))
            envelope = json.dumps({"op": "test.echo", "args": {"value": value}})
            writer.write(envelope.encode("utf-8") + b"\n")
            writer.write_eof()
            await writer.drain()
            raw = await reader.read()
            writer.close()
            await writer.wait_closed()
            return json.loads(raw.decode("utf-8").strip())

        first = await call(1)
        second = await call(2)
        assert first["value"] == 1
        assert second["value"] == 2
        # Per-connection ``boot_t0`` must keep ``boot_to_dispatch_s`` small
        # regardless of daemon uptime (regression guard for module-level
        # ``_DISPATCHER_BOOT_MONOTONIC`` leaking into daemon mode).
        assert second["timings"]["runtime.boot_to_dispatch_s"] < 0.05
    finally:
        serve_task.cancel()
        try:
            await serve_task
        except (asyncio.CancelledError, Exception):
            pass


async def test_daemon_serves_tcp_with_auth_token() -> None:
    socket_path, pid_path = _short_socket_path()
    tcp_port = _free_tcp_port()

    async def echo(args: dict[str, object]) -> dict[str, object]:
        return {"success": True, "value": args["value"]}

    server.register_op("test.echo", echo)

    serve_task = asyncio.create_task(
        daemon.serve(
            socket_path,
            pid_path,
            tcp_host="127.0.0.1",
            tcp_port=tcp_port,
            auth_token="secret",
        )
    )
    try:
        for _ in range(50):
            if socket_path.exists():
                break
            await asyncio.sleep(0.02)
        assert socket_path.exists(), "daemon never bound socket"

        async def call(envelope: dict[str, object]) -> dict[str, object]:
            reader, writer = await asyncio.open_connection("127.0.0.1", tcp_port)
            writer.write(json.dumps(envelope).encode("utf-8") + b"\n")
            if writer.can_write_eof():
                writer.write_eof()
            await writer.drain()
            raw = await reader.read()
            writer.close()
            await writer.wait_closed()
            return json.loads(raw.decode("utf-8").strip())

        unauthorized = await call({"op": "test.echo", "args": {"value": 1}})
        authorized = await call(
            {
                daemon.DAEMON_AUTH_FIELD: "secret",
                "op": "test.echo",
                "args": {"value": 2},
            }
        )

        assert unauthorized["success"] is False
        assert unauthorized["error"]["kind"] == "unauthorized"
        assert authorized["success"] is True
        assert authorized["value"] == 2
    finally:
        serve_task.cancel()
        try:
            await serve_task
        except (asyncio.CancelledError, Exception):
            pass


async def test_daemon_handles_invalid_json() -> None:
    socket_path, pid_path = _short_socket_path()
    serve_task = asyncio.create_task(daemon.serve(socket_path, pid_path))
    try:
        for _ in range(50):
            if socket_path.exists():
                break
            await asyncio.sleep(0.02)
        reader, writer = await asyncio.open_unix_connection(str(socket_path))
        writer.write(b"{not json\n")
        writer.write_eof()
        await writer.drain()
        raw = await reader.read()
        writer.close()
        await writer.wait_closed()
        response = json.loads(raw.decode("utf-8").strip())
        assert response["success"] is False
        assert response["error"]["kind"] == "bad_json"
    finally:
        serve_task.cancel()
        try:
            await serve_task
        except (asyncio.CancelledError, Exception):
            pass


def test_peer_bootstraps_register_snapshot_ops() -> None:
    server._register_builtin_operations()

    assert "api.acquire_snapshot" in server.OP_TABLE
    assert "api.release_lease" in server.OP_TABLE
    assert "api.release_workspace_snapshot" not in server.OP_TABLE


def test_services_cached_per_layer_stack_root(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """OCC runtime-service factory caches the per-root bundle across calls."""
    occ_runtime_services.clear_occ_runtime_services()

    class _FakeManager:
        def __init__(self, root: str) -> None:
            self.root = root

    monkeypatch.setattr(
        occ_runtime_services,
        "get_layer_stack_manager",
        lambda root: _FakeManager(str(root)),
    )
    monkeypatch.setattr(
        occ_runtime_services,
        "LayerStackPortAdapter",
        lambda manager: ("layer-stack", manager),
    )
    monkeypatch.setattr(
        occ_runtime_services,
        "SnapshotGitignoreOracle",
        lambda layer_stack: ("oracle", layer_stack),
    )
    monkeypatch.setattr(
        occ_runtime_services,
        "OccService",
        lambda *, gitignore, layer_stack, maintenance=None: (
            "service",
            gitignore,
            layer_stack,
            maintenance,
        ),
    )
    monkeypatch.setattr(
        occ_runtime_services,
        "OccClient",
        lambda service, *, binding_reader, workspace_ref: (
            "occ-client",
            service,
            workspace_ref,
        ),
    )

    a1 = occ_runtime_services.get_occ_runtime_services("/tmp/a")
    a2 = occ_runtime_services.get_occ_runtime_services("/tmp/a")
    b1 = occ_runtime_services.get_occ_runtime_services("/tmp/b")

    assert a1 is a2  # same root → cached tuple
    assert a1.layer_stack_manager is not b1.layer_stack_manager  # different roots → distinct managers


def test_drop_occ_runtime_services_removes_only_requested_root(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """The shared OCC runtime-service cache is owned by ``occ_runtime_services``."""
    occ_runtime_services.clear_occ_runtime_services()

    monkeypatch.setattr(
        occ_runtime_services,
        "get_layer_stack_manager",
        lambda _root: object(),
    )
    monkeypatch.setattr(
        occ_runtime_services,
        "LayerStackPortAdapter",
        lambda _manager: object(),
    )
    monkeypatch.setattr(
        occ_runtime_services,
        "SnapshotGitignoreOracle",
        lambda _layer_stack: object(),
    )
    monkeypatch.setattr(
        occ_runtime_services,
        "OccService",
        lambda *, gitignore, layer_stack, maintenance=None: object(),
    )
    monkeypatch.setattr(
        occ_runtime_services,
        "OccClient",
        lambda service, *, binding_reader, workspace_ref: object(),
    )

    a = occ_runtime_services.get_occ_runtime_services("/tmp/a")
    b = occ_runtime_services.get_occ_runtime_services("/tmp/b")

    occ_runtime_services.drop_occ_runtime_services("/tmp/a")

    assert occ_runtime_services.get_occ_runtime_services("/tmp/a") is not a
    assert occ_runtime_services.get_occ_runtime_services("/tmp/b") is b


def test_runtime_service_cache_close_paths_close_owned_occ_services(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    occ_runtime_services.clear_occ_runtime_services()
    closed: list[int] = []
    next_service_id = 0

    class _Service:
        def __init__(self) -> None:
            nonlocal next_service_id
            self.service_id = next_service_id
            next_service_id += 1

        def close(self) -> None:
            closed.append(self.service_id)

    monkeypatch.setattr(occ_runtime_services, "get_layer_stack_manager", lambda _root: object())
    monkeypatch.setattr(
        occ_runtime_services,
        "LayerStackPortAdapter",
        lambda _manager: object(),
    )
    monkeypatch.setattr(
        occ_runtime_services, "SnapshotGitignoreOracle", lambda _layer_stack: object()
    )
    monkeypatch.setattr(
        occ_runtime_services,
        "OccService",
        lambda *, gitignore, layer_stack, maintenance=None: _Service(),
    )
    monkeypatch.setattr(
        occ_runtime_services,
        "OccClient",
        lambda service, *, binding_reader, workspace_ref: object(),
    )

    a = occ_runtime_services.get_occ_runtime_services("/tmp/a")
    b = occ_runtime_services.get_occ_runtime_services("/tmp/b")

    occ_runtime_services.drop_occ_runtime_services("/tmp/a")
    assert closed == [a.occ_service.service_id]

    occ_runtime_services.clear_occ_runtime_services()
    assert closed == [a.occ_service.service_id, b.occ_service.service_id]


async def test_build_workspace_base_reset_drops_cache_without_drain(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    calls: list[tuple[str, str]] = []

    class _FakeBinding:
        def to_dict(self) -> dict[str, object]:
            return {"workspace_root": "/ephemeral-os"}

    def _fake_build_workspace_base(
        layer_stack_root: str,
        *,
        workspace_root: str,
        reset: bool,
        timings: dict[str, float],
    ) -> _FakeBinding:
        calls.append(("server", layer_stack_root))
        calls.append(("build", f"{workspace_root}|reset={reset}"))
        timings["server.build_workspace_base_s"] = 0.01
        return _FakeBinding()

    monkeypatch.setattr(layer_stack_runtime, "build_workspace_base", _fake_build_workspace_base)
    monkeypatch.setattr(
        occ_runtime_services,
        "drop_occ_runtime_services",
        lambda layer_stack_root: calls.append(("drop", layer_stack_root)),
    )
    monkeypatch.setattr(
        occ_runtime_services,
        "drain_backend_auto_squash",
        lambda layer_stack_root: calls.append(("drain", layer_stack_root)),
        raising=False,
    )

    result = await workspace_handler.build_workspace_base(
        {
            "layer_stack_root": "/tmp/stack",
            "workspace_root": "/ephemeral-os",
            "reset": True,
        }
    )

    assert result["success"] is True
    assert result["created"] is True
    assert result["binding"] == {"workspace_root": "/ephemeral-os"}
    assert ("drop", "/tmp/stack") in calls
    assert ("drain", "/tmp/stack") not in calls
    assert ("build", "/ephemeral-os|reset=True") in calls
