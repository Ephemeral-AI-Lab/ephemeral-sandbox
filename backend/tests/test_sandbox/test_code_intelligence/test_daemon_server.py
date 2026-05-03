"""Unit tests for the Phase 2 in-sandbox CI daemon."""

from __future__ import annotations

import asyncio
import os
import shutil
import signal
import struct
import tempfile
from collections.abc import Iterator
from pathlib import Path
from typing import Any

import pytest

from sandbox.code_intelligence.daemon.server import (
    DISPATCH,
    DaemonAlreadyRunning,
    _dispatch_request,
    handle_ping,
    handle_shutdown,
    run_daemon,
)
from sandbox.code_intelligence.daemon.protocol import (
    CI_PROTOCOL_VERSION,
    MAX_FRAME_BYTES,
    FrameError,
    SchemaError,
    encode_frame,
    parse_request,
    read_frame,
)
from sandbox.code_intelligence.daemon.storage import state_dir


@pytest.fixture
def short_home_workspace(
    monkeypatch: pytest.MonkeyPatch,
) -> Iterator[Path]:
    root = Path(tempfile.mkdtemp(prefix="eos-ci-", dir="/tmp"))
    home = root / "h"
    workspace = root / "w"
    home.mkdir()
    workspace.mkdir()
    monkeypatch.setenv("HOME", str(home))
    try:
        yield workspace
    finally:
        shutil.rmtree(root, ignore_errors=True)


async def _decode_frame(frame: bytes) -> dict[str, Any]:
    reader = asyncio.StreamReader()
    reader.feed_data(frame)
    reader.feed_eof()
    return await read_frame(reader)


@pytest.mark.asyncio
async def test_frame_round_trip() -> None:
    body = {
        "v": CI_PROTOCOL_VERSION,
        "id": "req-1",
        "ok": False,
        "error": {"kind": "UnsupportedOp", "message": "nope", "details": {}},
    }
    assert await _decode_frame(encode_frame(body)) == body


def test_encode_rejects_oversized_frame(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(
        "sandbox.code_intelligence.daemon.protocol.MAX_FRAME_BYTES",
        1,
    )
    with pytest.raises(FrameError, match="frame too large"):
        encode_frame({"v": CI_PROTOCOL_VERSION, "id": "x"})


@pytest.mark.asyncio
async def test_read_rejects_oversized_header() -> None:
    reader = asyncio.StreamReader()
    reader.feed_data(struct.pack(">I", MAX_FRAME_BYTES + 1))
    reader.feed_eof()
    with pytest.raises(FrameError, match="oversized frame header"):
        await read_frame(reader)


@pytest.mark.asyncio
async def test_read_rejects_bad_schema_version() -> None:
    with pytest.raises(SchemaError, match="bad schema version"):
        await _decode_frame(encode_frame({"v": 999, "id": "x"}))


def test_parse_request_rejects_bad_shape() -> None:
    with pytest.raises(SchemaError, match="request op"):
        parse_request({"v": CI_PROTOCOL_VERSION, "id": "req-1", "args": {}})


@pytest.mark.asyncio
async def test_dispatch_table_control_ops() -> None:
    assert {"ping", "shutdown", "version"}.issubset(DISPATCH)
    response = await _dispatch_request(
        {"v": CI_PROTOCOL_VERSION, "id": "req-1", "op": "nope", "args": {}}
    )
    assert response["ok"] is False
    assert response["error"]["kind"] == "UnsupportedOp"


@pytest.mark.asyncio
async def test_handle_ping_shape() -> None:
    result = await handle_ping({})
    assert result["pong"] is True
    assert isinstance(result["uptime_s"], float)


@pytest.mark.asyncio
async def test_handle_shutdown_schedules_sigterm(monkeypatch: pytest.MonkeyPatch) -> None:
    calls: list[tuple[int, int]] = []
    monkeypatch.setattr(os, "kill", lambda pid, sig: calls.append((pid, sig)))

    result = await handle_shutdown({})
    await asyncio.sleep(0.1)

    assert result == {"shutting_down": True}
    assert calls == [(os.getpid(), signal.SIGTERM)]


async def _wait_for_socket(path: Path) -> None:
    for _ in range(50):
        if path.is_socket():
            return
        await asyncio.sleep(0.02)
    raise AssertionError(f"socket did not appear: {path}")


@pytest.mark.asyncio
async def test_run_daemon_serves_ping_and_cleans_up(
    short_home_workspace: Path,
) -> None:
    workspace = short_home_workspace
    state = state_dir(str(workspace))
    socket_path = state / "daemon.sock"
    pid_path = state / "daemon.pid"

    task = asyncio.create_task(run_daemon(str(workspace)))
    try:
        await _wait_for_socket(socket_path)
        reader, writer = await asyncio.open_unix_connection(str(socket_path))
        writer.write(
            encode_frame(
                {
                    "v": CI_PROTOCOL_VERSION,
                    "id": "req-1",
                    "op": "ping",
                    "args": {},
                }
            )
        )
        await writer.drain()
        response = await read_frame(reader)
        writer.close()
        await writer.wait_closed()

        assert response["ok"] is True
        assert response["result"]["pong"] is True
        assert pid_path.exists()
    finally:
        task.cancel()
        with pytest.raises(asyncio.CancelledError):
            await task

    assert not socket_path.exists()
    assert not pid_path.exists()


@pytest.mark.asyncio
async def test_dead_stale_pid_is_unlinked_and_replaced(
    short_home_workspace: Path,
) -> None:
    workspace = short_home_workspace
    state = state_dir(str(workspace))
    socket_path = state / "daemon.sock"
    pid_path = state / "daemon.pid"
    pid_path.write_text("999999999\n", encoding="utf-8")
    socket_path.write_text("stale", encoding="utf-8")

    task = asyncio.create_task(run_daemon(str(workspace)))
    try:
        await _wait_for_socket(socket_path)
        assert pid_path.read_text(encoding="utf-8").strip() == str(os.getpid())
        assert socket_path.is_socket()
    finally:
        task.cancel()
        with pytest.raises(asyncio.CancelledError):
            await task


def test_live_stale_pid_exits_11(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("HOME", str(tmp_path / "home"))
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    state = state_dir(str(workspace))
    (state / "daemon.pid").write_text(f"{os.getpid()}\n", encoding="utf-8")

    with pytest.raises(DaemonAlreadyRunning):
        asyncio.run(run_daemon(str(workspace)))
