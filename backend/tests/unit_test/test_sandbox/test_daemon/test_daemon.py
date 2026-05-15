"""Tests for the resident sandbox daemon."""

from __future__ import annotations

import asyncio
import json
import os
import tempfile
import uuid
from pathlib import Path

import pytest

from sandbox.daemon import _toolbox as request_context
from sandbox.daemon.handler import workspace as workspace_handler
from sandbox.daemon.rpc import dispatcher as server
from sandbox.daemon.rpc import server as daemon
from sandbox.daemon import occ_backend, workspace_server


def _short_socket_path() -> tuple[Path, Path]:
    """Return ``(socket, pid)`` paths short enough for AF_UNIX (≤104 bytes)."""
    base = Path(tempfile.gettempdir()) / f"eos-daemon-{uuid.uuid4().hex[:8]}"
    base.mkdir(parents=True, exist_ok=True)
    return base / "runtime.sock", base / "runtime.pid"


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


async def test_dispatch_envelope_async_honors_boot_t0_override() -> None:
    """``boot_t0`` overrides module-level ``_BOOT_T0`` so daemon-mode dispatch
    measures per-call boot, not daemon uptime."""
    from sandbox._shared.clock import monotonic_now

    def handler(_: dict[str, object]) -> dict[str, object]:
        return {"success": True}

    server.register_op("test.boot", handler)

    # Pretend the daemon has been running for hours: real `_BOOT_T0` is far
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
        # ``_BOOT_T0`` leaking into daemon mode).
        assert second["timings"]["runtime.boot_to_dispatch_s"] < 0.05
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
    server._load_peer_bootstraps()

    assert "api.prepare_workspace_snapshot" in server.OP_TABLE
    assert "api.release_workspace_snapshot" in server.OP_TABLE


def test_services_cached_per_layer_stack_root(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """OCC backend factory caches the per-root tuple across calls."""
    occ_backend.clear_backend_cache()

    class _FakeManager:
        def __init__(self, root: str) -> None:
            self.root = root

    monkeypatch.setattr(
        occ_backend,
        "get_layer_stack_manager",
        lambda root: _FakeManager(str(root)),
    )
    monkeypatch.setattr(
        occ_backend,
        "LayerStackClient",
        lambda manager: ("layer-stack", manager),
    )
    monkeypatch.setattr(
        occ_backend,
        "SnapshotGitignoreOracle",
        lambda layer_stack: ("oracle", layer_stack),
    )
    monkeypatch.setattr(
        occ_backend,
        "OccService",
        lambda *, gitignore, layer_stack, maintenance=None: (
            "service",
            gitignore,
            layer_stack,
            maintenance,
        ),
    )
    monkeypatch.setattr(
        occ_backend,
        "OccClient",
        lambda service, *, binding_reader, workspace_ref: (
            "occ-client",
            service,
            workspace_ref,
        ),
    )

    a1 = request_context.services("/tmp/a")
    a2 = request_context.services("/tmp/a")
    b1 = request_context.services("/tmp/b")

    assert a1 is a2  # same root → cached tuple
    assert a1.manager is not b1.manager  # different roots → distinct managers


def test_drop_backend_cache_removes_only_requested_root(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """The shared OCC backend cache is owned directly by ``occ_backend``."""
    occ_backend.clear_backend_cache()

    monkeypatch.setattr(
        occ_backend,
        "get_layer_stack_manager",
        lambda _root: object(),
    )
    monkeypatch.setattr(occ_backend, "LayerStackClient", lambda _manager: object())
    monkeypatch.setattr(
        occ_backend,
        "SnapshotGitignoreOracle",
        lambda _layer_stack: object(),
    )
    monkeypatch.setattr(
        occ_backend,
        "OccService",
        lambda *, gitignore, layer_stack, maintenance=None: object(),
    )
    monkeypatch.setattr(
        occ_backend,
        "OccClient",
        lambda service, *, binding_reader, workspace_ref: object(),
    )

    a = occ_backend.build_occ_backend("/tmp/a")
    b = occ_backend.build_occ_backend("/tmp/b")

    occ_backend.drop_backend_cache("/tmp/a")

    assert occ_backend.build_occ_backend("/tmp/a") is not a
    assert occ_backend.build_occ_backend("/tmp/b") is b


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

    monkeypatch.setattr(workspace_server, "build_workspace_base", _fake_build_workspace_base)
    monkeypatch.setattr(
        occ_backend,
        "drop_backend_cache",
        lambda layer_stack_root: calls.append(("drop", layer_stack_root)),
    )
    monkeypatch.setattr(
        occ_backend,
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
