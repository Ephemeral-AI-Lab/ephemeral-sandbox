"""Tests for public sandbox API audit emission."""

from __future__ import annotations

import pytest

from audit.bus import AuditEventBus
from sandbox.api import (
    EditFileRequest,
    ExecCommandRequest,
    ReadFileRequest,
    SandboxCaller,
    SearchReplaceEdit,
    WriteFileRequest,
)
from sandbox.audit import events
import sandbox.api.tool.command as command_module
import sandbox.api.tool.edit as edit_module
import sandbox.api.tool.read as read_module
import sandbox.api.tool.write as write_module


@pytest.mark.asyncio
async def test_read_file_publishes_started_and_completed(
    monkeypatch: pytest.MonkeyPatch,
    recording_transport_factory,
) -> None:
    bus = AuditEventBus()
    published = []
    bus.subscribe(published.append)

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
        ReadFileRequest(
            path="a.txt",
            caller=SandboxCaller(
                agent_id="agent-1",
                request_id="request-1",
                task_id="task-1",
                tool_id="tool-1",
            ),
        ),
        audit_sink=bus,
        transport=transport,
    )

    assert result.content == "hello"
    assert [event.type for event in published] == [
        events.OPERATION_STARTED,
        events.OPERATION_COMPLETED,
    ]
    assert published[0].payload == {"operation": "read_file", "path": "a.txt"}
    assert published[1].node.request_id == "request-1"
    assert published[1].node.task_id == "task-1"
    assert published[1].node.tool_name == "read_file"
    assert published[1].node.tool_use_id == "tool-1"
    assert published[1].payload["timings"] == {"api.read.total_s": 0.1}


@pytest.mark.asyncio
async def test_write_conflict_publishes_one_operation_conflict(
    monkeypatch: pytest.MonkeyPatch,
    recording_transport_factory,
) -> None:
    bus = AuditEventBus()
    published = []
    bus.subscribe(published.append)

    async def fake_call_daemon_api(sandbox_id, op, args, timeout):
        del sandbox_id, op, args, timeout
        return {
            "success": False,
            "changed_paths": [],
            "status": "aborted_version",
            "conflict": {
                "reason": "aborted_version",
                "conflict_file": "a.py",
                "message": "base mismatch",
            },
            "conflict_reason": "base mismatch",
            "timings": {"occ.prepare.total_s": 0.01, "occ.apply.total_s": 0.02},
        }

    del monkeypatch
    transport = recording_transport_factory(fake_call_daemon_api)

    result = await write_module.write_file(
        "sb-1",
        WriteFileRequest(
            path="a.py",
            content="x",
            caller=SandboxCaller(agent_id="agent-1"),
        ),
        audit_sink=bus,
        transport=transport,
    )

    assert result.success is False
    operation_events = [
        event for event in published if event.type.startswith("sandbox.operation.")
    ]
    assert [event.type for event in operation_events] == [
        events.OPERATION_STARTED,
        events.OPERATION_CONFLICTED,
    ]
    assert [event.type for event in published] == [
        events.OPERATION_STARTED,
        events.OPERATION_CONFLICTED,
        events.OCC_PREPARED,
        events.OCC_CONFLICTED,
    ]
    assert published[1].payload["status"] == "conflict"
    assert published[1].payload["conflict_reason"] == "base mismatch"


@pytest.mark.asyncio
async def test_edit_anchor_error_publishes_operation_conflict(
    monkeypatch: pytest.MonkeyPatch,
    recording_transport_factory,
) -> None:
    bus = AuditEventBus()
    published = []
    bus.subscribe(published.append)

    async def fake_call_daemon_api(sandbox_id, op, args, timeout):
        del sandbox_id, args, timeout
        if op == "api.v1.read_file":
            return {
                "success": True,
                "exists": True,
                "content": "missing",
                "encoding": "utf-8",
                "timings": {},
            }
        raise RuntimeError("anchor not found in a.py: expected 1 occurrences")

    del monkeypatch
    transport = recording_transport_factory(fake_call_daemon_api)

    result = await edit_module.edit_file(
        "sb-1",
        EditFileRequest(
            path="a.py",
            edits=(SearchReplaceEdit(old_text="missing", new_text="x"),),
            caller=SandboxCaller(agent_id="agent-1"),
        ),
        audit_sink=bus,
        transport=transport,
    )

    assert result.success is False
    assert result.status == "aborted_overlap"
    assert [event.type for event in published] == [
        events.OPERATION_STARTED,
        events.OPERATION_CONFLICTED,
    ]
    assert published[1].payload["status"] == "conflict"
    assert published[1].payload["conflict_reason"] == (
        "anchor not found in a.py: expected 1 occurrences"
    )


@pytest.mark.asyncio
async def test_exec_command_publishes_started_and_completed(
    monkeypatch: pytest.MonkeyPatch,
    recording_transport_factory,
) -> None:
    bus = AuditEventBus()
    published = []
    bus.subscribe(published.append)

    async def fake_call_daemon_api(sandbox_id, op, args, timeout):
        del sandbox_id, op, args, timeout
        return {
            "status": "ok",
            "exit_code": 0,
            "output": {"stdout": "done\n", "stderr": ""},
            "timings": {"api.exec_command.total_s": 0.1},
        }

    del monkeypatch
    transport = recording_transport_factory(fake_call_daemon_api)

    result = await command_module.exec_command(
        "sb-command",
        ExecCommandRequest(
            cmd="printf done",
            caller=SandboxCaller(agent_id="agent-1"),
        ),
        audit_sink=bus,
        transport=transport,
    )

    assert result.success is True
    assert result.output.stdout == "done\n"
    assert [event.type for event in published] == [
        events.OPERATION_STARTED,
        events.OPERATION_COMPLETED,
    ]
    assert published[0].payload["operation"] == "exec_command"
    assert published[1].payload["status"] == "ok"
