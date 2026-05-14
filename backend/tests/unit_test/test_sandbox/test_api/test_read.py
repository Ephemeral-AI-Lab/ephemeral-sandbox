"""Tests for ``sandbox.api.tool.read``."""

from __future__ import annotations

import pytest

from sandbox.api import ReadFileRequest, SandboxCaller
import sandbox.api.tool.read as read_module


@pytest.mark.asyncio
async def test_read_file_dispatches_to_sandbox_daemon(
    monkeypatch: pytest.MonkeyPatch,
    recording_transport_factory,
) -> None:
    async def fake_call_daemon_api(sandbox_id, op, args, timeout):
        del sandbox_id, op, args, timeout
        return {
            "success": True,
            "exists": True,
            "content": "hello",
            "encoding": "utf-8",
            "timings": {"api.read.total_s": 0.1},
        }

    del monkeypatch
    transport = recording_transport_factory(fake_call_daemon_api)

    result = await read_module.read_file(
        "sb-1",
        ReadFileRequest(path="/workspace/a.txt", caller=SandboxCaller(agent_id="a")),
        transport=transport,
    )

    assert result.success is True
    assert result.exists is True
    assert result.content == "hello"
    assert not hasattr(result, "conflict")
    assert transport.calls == [
        (
            "sb-1",
            "api.v1.read_file",
            {
                "path": "/workspace/a.txt",
                "caller": {
                    "agent_id": "a",
                    "run_id": "",
                    "agent_run_id": "",
                    "task_id": "",
                },
            },
            60,
        ),
    ]


@pytest.mark.asyncio
async def test_read_file_missing_file_maps_to_exists_false(
    monkeypatch: pytest.MonkeyPatch,
    recording_transport_factory,
) -> None:
    async def fake_call_daemon_api(sandbox_id, op, args, timeout):
        del sandbox_id, op, args, timeout
        return {
            "success": True,
            "exists": False,
            "content": "",
            "encoding": "utf-8",
            "timings": {},
        }

    del monkeypatch
    transport = recording_transport_factory(fake_call_daemon_api)

    result = await read_module.read_file(
        "sb-1",
        ReadFileRequest(path="/missing", caller=SandboxCaller(agent_id="a")),
        transport=transport,
    )

    assert result.success is True
    assert result.exists is False
    assert result.content == ""
