"""Tests for ``sandbox.api.tool.edit``."""

from __future__ import annotations

import pytest

from sandbox.api import EditFileRequest, SandboxCaller, SearchReplaceEdit
from sandbox.api.tool.edit import edit_file


@pytest.mark.asyncio
async def test_edit_file_dispatches_to_sandbox_daemon(
    monkeypatch: pytest.MonkeyPatch,
    recording_transport_factory,
) -> None:
    async def fake_call_daemon_api(sandbox_id, op, args, timeout):
        del sandbox_id, args, timeout
        if op == "api.v1.read_file":
            return {
                "success": True,
                "exists": True,
                "content": "old",
                "encoding": "utf-8",
                "timings": {},
            }
        return {
            "success": True,
            "changed_paths": ["a.py"],
            "applied_edits": 1,
            "status": "ok",
            "conflict": None,
            "conflict_reason": None,
            "timings": {},
        }

    del monkeypatch
    transport = recording_transport_factory(fake_call_daemon_api)

    result = await edit_file(
        "sb-edit",
        EditFileRequest(
            path="a.py",
            edits=(SearchReplaceEdit(old_text="old", new_text="new"),),
            caller=SandboxCaller(agent_id="agent-1"),
            description="edit a",
        ),
        transport=transport,
    )

    assert result.success is True
    assert result.changed_paths == ("a.py",)
    assert result.applied_edits == 1
    assert transport.calls == [
        (
            "sb-edit",
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
            "sb-edit",
            "api.v1.edit_file",
            {
                "path": "a.py",
                "edits": [{"old_text": "old", "new_text": "new"}],
                "actor_id": "agent-1",
                "caller": {
                    "agent_id": "agent-1",
                    "run_id": "",
                    "agent_run_id": "",
                    "task_id": "",
                },
                "description": "edit a",
            },
            20,
        )
    ]


@pytest.mark.asyncio
async def test_edit_file_recovers_when_transient_exec_already_applied_edit(
    monkeypatch: pytest.MonkeyPatch,
    recording_transport_factory,
) -> None:
    calls: list[tuple[str, int]] = []
    old_text = "# marker\n"
    new_text = "# marker\nclass RetryPolicy:\n    pass\n"

    async def fake_call_daemon_api(sandbox_id, op, args, timeout):
        del sandbox_id, args
        calls.append((op, timeout))
        if calls == [("api.v1.read_file", 20)]:
            return {
                "success": True,
                "exists": True,
                "content": old_text,
                "encoding": "utf-8",
                "timings": {},
            }
        if op == "api.v1.edit_file":
            raise RuntimeError("DaytonaError: Failed to execute command: ")
        if op == "api.v1.read_file":
            return {
                "success": True,
                "exists": True,
                "content": new_text,
                "encoding": "utf-8",
                "timings": {},
            }
        raise AssertionError(op)

    del monkeypatch
    transport = recording_transport_factory(fake_call_daemon_api)

    result = await edit_file(
        "sb-edit-transient",
        EditFileRequest(
            path="retry.py",
            edits=(SearchReplaceEdit(old_text=old_text, new_text=new_text),),
            caller=SandboxCaller(agent_id="agent-1"),
        ),
        transport=transport,
    )

    assert result.success is True
    assert result.changed_paths == ("retry.py",)
    assert result.applied_edits == 1
    assert result.status == "edited"
    assert result.timings["api.edit.recovered_after_transient"] == 1.0
    assert calls == [
        ("api.v1.read_file", 20),
        ("api.v1.edit_file", 20),
        ("api.v1.read_file", 20),
    ]


@pytest.mark.asyncio
async def test_edit_file_retries_transient_exec_when_edit_not_visible(
    monkeypatch: pytest.MonkeyPatch,
    recording_transport_factory,
) -> None:
    calls: list[tuple[str, int]] = []

    async def fake_call_daemon_api(sandbox_id, op, args, timeout):
        del sandbox_id, args
        calls.append((op, timeout))
        if calls == [("api.v1.read_file", 20)]:
            return {
                "success": True,
                "exists": True,
                "content": "old",
                "encoding": "utf-8",
                "timings": {},
            }
        if calls == [("api.v1.read_file", 20), ("api.v1.edit_file", 20)]:
            raise RuntimeError("DaytonaError: Failed to execute command: ")
        if op == "api.v1.read_file":
            return {
                "success": True,
                "exists": True,
                "content": "old",
                "encoding": "utf-8",
                "timings": {},
            }
        if op == "api.v1.edit_file":
            return {
                "success": True,
                "changed_paths": ["a.py"],
                "applied_edits": 1,
                "status": "ok",
                "conflict": None,
                "conflict_reason": None,
                "timings": {},
            }
        raise AssertionError(op)

    del monkeypatch
    transport = recording_transport_factory(fake_call_daemon_api)

    result = await edit_file(
        "sb-edit-transient",
        EditFileRequest(
            path="a.py",
            edits=(SearchReplaceEdit(old_text="old", new_text="new"),),
            caller=SandboxCaller(agent_id="agent-1"),
        ),
        transport=transport,
    )

    assert result.success is True
    assert result.changed_paths == ("a.py",)
    assert calls == [
        ("api.v1.read_file", 20),
        ("api.v1.edit_file", 20),
        ("api.v1.read_file", 20),
        ("api.v1.edit_file", 20),
    ]


@pytest.mark.asyncio
async def test_edit_file_guard_failure_maps_conflict_info(
    monkeypatch: pytest.MonkeyPatch,
    recording_transport_factory,
) -> None:
    async def fake_call_daemon_api(sandbox_id, op, args, timeout):
        del sandbox_id, args, timeout
        if op == "api.v1.read_file":
            return {
                "success": True,
                "exists": True,
                "content": "old",
                "encoding": "utf-8",
                "timings": {},
            }
        return {
            "success": False,
            "changed_paths": [],
            "applied_edits": 0,
            "status": "aborted_overlap",
            "conflict": {
                "reason": "aborted_overlap",
                "conflict_file": "a.py",
                "message": "patch_failed",
            },
            "conflict_reason": "patch_failed",
            "timings": {},
        }

    del monkeypatch
    transport = recording_transport_factory(fake_call_daemon_api)

    result = await edit_file(
        "sb-edit-conflict",
        EditFileRequest(
            path="a.py",
            edits=(SearchReplaceEdit(old_text="old", new_text="new"),),
            caller=SandboxCaller(agent_id="agent-1"),
        ),
        transport=transport,
    )

    assert result.success is False
    assert result.applied_edits == 0
    assert result.status == "aborted_overlap"
    assert result.conflict is not None
    assert result.conflict.reason == "aborted_overlap"
    assert result.conflict.message == "patch_failed"
    assert result.conflict_reason == "patch_failed"


@pytest.mark.asyncio
async def test_edit_file_anchor_error_maps_to_conflict_result(
    monkeypatch: pytest.MonkeyPatch,
    recording_transport_factory,
) -> None:
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
        raise RuntimeError(
            "internal_error: anchor not found in a.py: expected 1 occurrences"
        )

    del monkeypatch
    transport = recording_transport_factory(fake_call_daemon_api)

    result = await edit_file(
        "sb-edit-conflict",
        EditFileRequest(
            path="a.py",
            edits=(SearchReplaceEdit(old_text="missing", new_text="new"),),
            caller=SandboxCaller(agent_id="agent-1"),
        ),
        transport=transport,
    )

    assert result.success is False
    assert result.applied_edits == 0
    assert result.status == "aborted_overlap"
    assert result.conflict is not None
    assert result.conflict.reason == "aborted_overlap"
    assert result.conflict.conflict_file == "a.py"
    assert result.conflict_reason == (
        "anchor not found in a.py: expected 1 occurrences"
    )
