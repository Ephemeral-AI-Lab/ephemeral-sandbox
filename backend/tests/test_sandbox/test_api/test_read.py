"""Tests for ``sandbox.api.read``."""

from __future__ import annotations

from sandbox.api.utils.models import RawExecResult, ReadFileRequest, RequestActor
import sandbox.api.read as read_module


async def test_read_file_uses_raw_exec_and_maps_content(monkeypatch) -> None:
    calls: list[tuple[str, str]] = []

    async def fake_raw_exec(sandbox_id: str, command: str):
        calls.append((sandbox_id, command))
        return RawExecResult(
            exit_code=0,
            stdout='{"exists": true, "content": "hello"}',
        )

    monkeypatch.setattr(read_module, "raw_exec", fake_raw_exec)

    result = await read_module.read_file(
        "sb-1",
        ReadFileRequest(path="/workspace/a.txt", actor=RequestActor(agent_id="a")),
    )

    assert result.success is True
    assert result.exists is True
    assert result.content == "hello"
    assert not hasattr(result, "conflict")
    assert calls and calls[0][0] == "sb-1"
    assert "sandbox.runtime.server" not in calls[0][1]


async def test_read_file_missing_file_maps_to_exists_false(monkeypatch) -> None:
    async def fake_raw_exec(sandbox_id: str, command: str):
        del sandbox_id, command
        return RawExecResult(
            exit_code=0,
            stdout='{"exists": false, "content": ""}',
        )

    monkeypatch.setattr(read_module, "raw_exec", fake_raw_exec)

    result = await read_module.read_file(
        "sb-1",
        ReadFileRequest(path="/missing", actor=RequestActor(agent_id="a")),
    )

    assert result.success is True
    assert result.exists is False
    assert result.content == ""
