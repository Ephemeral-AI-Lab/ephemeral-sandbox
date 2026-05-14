"""Tests for ``sandbox.api.tool.write``."""

from __future__ import annotations

import pytest

from sandbox.api import SandboxCaller, WriteFileRequest
from sandbox.api.tool.write import write_file


@pytest.mark.asyncio
async def test_write_file_dispatches_to_sandbox_daemon(
    monkeypatch: pytest.MonkeyPatch,
    recording_transport_factory,
) -> None:
    async def fake_call_daemon_api(sandbox_id, op, args, timeout):
        del sandbox_id, op, args, timeout
        return {
            "success": True,
            "changed_paths": ["a.py"],
            "status": "ok",
            "conflict": None,
            "conflict_reason": None,
            "timings": {"api.write.total_s": 0.1},
        }

    del monkeypatch
    transport = recording_transport_factory(fake_call_daemon_api)

    result = await write_file(
        "sb-write",
        WriteFileRequest(
            path="a.py",
            content="x",
            caller=SandboxCaller(agent_id="agent-1"),
            description="write a",
            overwrite=False,
        ),
        transport=transport,
    )

    assert result.success is True
    assert result.changed_paths == ("a.py",)
    assert result.timings["api.write.total_s"] == 0.1
    assert transport.calls == [
        (
            "sb-write",
            "api.v1.read_file",
            {
                "path": "a.py",
                "caller": {
                    "agent_id": "agent-1",
                    "run_id": "",
                    "agent_run_id": "",
                    "task_id": "",
                },
            },
            20,
        ),
        (
            "sb-write",
            "api.v1.write_file",
            {
                "path": "a.py",
                "content": "x",
                "actor_id": "agent-1",
                "caller": {
                    "agent_id": "agent-1",
                    "run_id": "",
                    "agent_run_id": "",
                    "task_id": "",
                },
                "description": "write a",
                "overwrite": False,
            },
            60,
        )
    ]


@pytest.mark.asyncio
async def test_write_file_recovers_when_transient_exec_already_applied_write(
    monkeypatch: pytest.MonkeyPatch,
    recording_transport_factory,
) -> None:
    calls: list[str] = []

    async def fake_call_daemon_api(sandbox_id, op, args, timeout):
        del sandbox_id, args, timeout
        calls.append(op)
        if calls == ["api.v1.read_file"]:
            return {
                "success": True,
                "exists": True,
                "content": "old",
                "encoding": "utf-8",
                "timings": {},
            }
        if op == "api.v1.write_file":
            raise RuntimeError("DaytonaError: Failed to execute command")
        if op == "api.v1.read_file":
            return {
                "success": True,
                "exists": True,
                "content": "new",
                "encoding": "utf-8",
                "timings": {},
            }
        raise AssertionError(op)

    del monkeypatch
    transport = recording_transport_factory(fake_call_daemon_api)

    result = await write_file(
        "sb-write-transient",
        WriteFileRequest(
            path="a.py",
            content="new",
            caller=SandboxCaller(agent_id="agent-1"),
        ),
        transport=transport,
    )

    assert result.success is True
    assert result.changed_paths == ("a.py",)
    assert result.status == "written"
    assert result.timings["api.write.recovered_after_transient"] == 1.0
    assert calls == ["api.v1.read_file", "api.v1.write_file", "api.v1.read_file"]


@pytest.mark.asyncio
async def test_write_file_does_not_recover_when_preimage_already_matched(
    monkeypatch: pytest.MonkeyPatch,
    recording_transport_factory,
) -> None:
    calls: list[str] = []

    async def fake_call_daemon_api(sandbox_id, op, args, timeout):
        del sandbox_id, args, timeout
        calls.append(op)
        if calls == ["api.v1.read_file"]:
            return {
                "success": True,
                "exists": True,
                "content": "same",
                "encoding": "utf-8",
                "timings": {},
            }
        if calls == ["api.v1.read_file", "api.v1.write_file"]:
            raise RuntimeError("DaytonaError: Failed to execute command")
        if op == "api.v1.write_file":
            return {
                "success": True,
                "changed_paths": ["a.py"],
                "status": "ok",
                "conflict": None,
                "conflict_reason": None,
                "timings": {},
            }
        raise AssertionError(op)

    del monkeypatch
    transport = recording_transport_factory(fake_call_daemon_api)

    result = await write_file(
        "sb-write-transient",
        WriteFileRequest(
            path="a.py",
            content="same",
            caller=SandboxCaller(agent_id="agent-1"),
        ),
        transport=transport,
    )

    assert result.success is True
    assert calls == ["api.v1.read_file", "api.v1.write_file", "api.v1.write_file"]


@pytest.mark.asyncio
async def test_write_file_guard_failure_maps_conflict_info(
    monkeypatch: pytest.MonkeyPatch,
    recording_transport_factory,
) -> None:
    async def fake_call_daemon_api(sandbox_id, op, args, timeout):
        del sandbox_id, op, args, timeout
        return {
            "success": False,
            "changed_paths": [],
            "status": "aborted_version",
            "conflict": {
                "reason": "aborted_version",
                "conflict_file": "a.py",
                "message": "base_mismatch",
            },
            "conflict_reason": "base_mismatch",
            "timings": {},
        }

    del monkeypatch
    transport = recording_transport_factory(fake_call_daemon_api)

    result = await write_file(
        "sb-write-conflict",
        WriteFileRequest(
            path="a.py",
            content="x",
            caller=SandboxCaller(agent_id="agent-1"),
        ),
        transport=transport,
    )

    assert result.success is False
    assert result.status == "aborted_version"
    assert result.conflict is not None
    assert result.conflict.reason == "aborted_version"
    assert result.conflict.conflict_file == "a.py"
    assert result.conflict.message == "base_mismatch"
    assert result.conflict_reason == "base_mismatch"
