"""Tests for the ``sandbox.api`` facade object."""

from __future__ import annotations

import pytest

from sandbox.api import (
    EditFileRequest,
    RawExecResult,
    ReadFileRequest,
    SandboxClient,
    SandboxCaller,
    ShellRequest,
    WriteFileRequest,
)


@pytest.mark.asyncio
async def test_tool_methods_delegate_to_backing_modules(
    monkeypatch: pytest.MonkeyPatch,
    recording_transport_factory,
) -> None:
    actor = SandboxCaller(agent_id="agent-1")
    raw_calls: list[tuple[tuple[object, ...], dict[str, object]]] = []

    async def fake_raw_exec(*args, **kwargs):
        raw_calls.append((args, kwargs))
        return RawExecResult(success=True, exit_code=0, stdout="raw")

    async def fake_transport_call(sandbox_id, op, args, timeout):
        del sandbox_id, args, timeout
        if op == "api.v1.shell":
            return {
                "success": True,
                "exit_code": 0,
                "stdout": "ok",
                "stderr": "",
                "changed_paths": [],
                "status": "ok",
                "conflict": None,
                "conflict_reason": None,
                "warnings": [],
                "timings": {},
            }
        if op == "api.v1.read_file":
            return {
                "success": True,
                "exists": True,
                "content": "content",
                "encoding": "utf-8",
                "timings": {},
            }
        if op == "api.v1.write_file":
            return {
                "success": True,
                "changed_paths": ["a.py"],
                "status": "ok",
                "conflict": None,
                "conflict_reason": None,
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

    monkeypatch.setattr("sandbox.api._impl.raw_exec.raw_exec", fake_raw_exec)
    transport = recording_transport_factory(fake_transport_call)
    facade = SandboxClient(transport=transport)

    shell_request = ShellRequest(command="pwd", caller=actor)
    read_request = ReadFileRequest(path="a.py", caller=actor)
    write_request = WriteFileRequest(path="a.py", content="x", caller=actor)
    edit_request = EditFileRequest(path="a.py", edits=(), caller=actor)

    assert (await facade.shell("sb-1", shell_request)).stdout == "ok"
    assert (await facade.raw_exec("sb-1", "pwd", cwd="/ws", timeout=5)).stdout == "raw"
    assert (await facade.read_file("sb-1", read_request)).content == "content"
    assert (await facade.write_file("sb-1", write_request)).changed_paths == ("a.py",)
    assert (await facade.edit_file("sb-1", edit_request)).applied_edits == 1

    assert raw_calls == [
        (("sb-1", "pwd"), {"cwd": "/ws", "timeout": 5, "audit_sink": None}),
    ]
    assert [call[1] for call in transport.calls] == [
        "api.v1.shell",
        "api.v1.read_file",
        "api.v1.read_file",
        "api.v1.write_file",
        "api.v1.edit_file",
    ]


def test_status_methods_delegate_to_status_module(monkeypatch: pytest.MonkeyPatch) -> None:
    facade = SandboxClient()
    calls: list[tuple[str, tuple[object, ...], dict[str, object]]] = []

    def record(name: str, value):
        def _inner(*args, **kwargs):
            calls.append((name, args, kwargs))
            return value

        return _inner

    monkeypatch.setattr("sandbox.api.status.create_sandbox", record("create", {"id": "sb"}))
    monkeypatch.setattr("sandbox.api.status.start_sandbox", record("start", {"state": "started"}))
    monkeypatch.setattr("sandbox.api.status.stop_sandbox", record("stop", {"state": "stopped"}))
    monkeypatch.setattr("sandbox.api.status.delete_sandbox", record("delete", None))
    monkeypatch.setattr("sandbox.api.status.ensure_sandbox_running", record("ensure", {"state": "started"}))
    monkeypatch.setattr("sandbox.api.status.set_sandbox_labels", record("labels", {"id": "sb"}))
    monkeypatch.setattr("sandbox.api.status.get_sandbox", record("get", {"id": "sb"}))
    monkeypatch.setattr("sandbox.api.status.list_sandboxes", record("list", [{"id": "sb"}]))
    monkeypatch.setattr("sandbox.api.status.list_snapshots", record("snapshots", [{"name": "snap"}]))
    monkeypatch.setattr("sandbox.api.status.get_health", record("health", {"available": True}))
    monkeypatch.setattr("sandbox.api.status.get_signed_preview_url", record("preview", {"url": "u"}))
    monkeypatch.setattr("sandbox.api.status.get_build_logs_url", record("logs", "log-url"))

    assert facade.create_sandbox(name="n", labels={"k": "v"}) == {"id": "sb"}
    assert facade.start_sandbox("sb") == {"state": "started"}
    assert facade.stop_sandbox("sb") == {"state": "stopped"}
    assert facade.delete_sandbox("sb") is None
    assert facade.ensure_sandbox_running("sb") == {"state": "started"}
    assert facade.set_sandbox_labels("sb", {"k": "v"}) == {"id": "sb"}
    assert facade.get_sandbox("sb") == {"id": "sb"}
    assert facade.list_sandboxes() == [{"id": "sb"}]
    assert facade.list_snapshots() == [{"name": "snap"}]
    assert facade.get_health() == {"available": True}
    assert facade.get_signed_preview_url("sb", 5173) == {"url": "u"}
    assert facade.get_build_logs_url("sb") == "log-url"

    assert calls == [
        ("create", (), {"name": "n", "snapshot": None, "image": None, "language": "python", "env_vars": None, "labels": {"k": "v"}}),
        ("start", ("sb",), {}),
        ("stop", ("sb",), {}),
        ("delete", ("sb",), {}),
        ("ensure", ("sb",), {}),
        ("labels", ("sb", {"k": "v"}), {}),
        ("get", ("sb",), {}),
        ("list", (), {}),
        ("snapshots", (), {}),
        ("health", (), {}),
        ("preview", ("sb", 5173), {}),
        ("logs", ("sb",), {}),
    ]


def test_context_preparer_delegates_to_control_factory(
) -> None:
    preparer = object()
    client = SandboxClient(
        context_preparer=lambda sandbox_id: preparer if sandbox_id == "sb-1" else None,
    )

    assert client.context_preparer_for("sb-1") is preparer
