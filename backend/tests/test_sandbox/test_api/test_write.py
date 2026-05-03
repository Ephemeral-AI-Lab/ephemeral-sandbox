"""Tests for ``sandbox.api.write``."""

from __future__ import annotations

import json
import shlex

from sandbox.api.models import RawExecResult, RequestActor, WriteFileRequest
from sandbox.api.write import write_file
from sandbox.providers.registry import dispose_adapter, register_adapter


class _Adapter:
    name = "write-api"

    def __init__(self, *, response: dict) -> None:
        self.response = response
        self.calls: list[tuple[str, str, str | None, int | None]] = []

    async def exec(
        self,
        sandbox_id: str,
        command: str,
        *,
        cwd: str | None = None,
        timeout: int | None = None,
    ) -> RawExecResult:
        self.calls.append((sandbox_id, command, cwd, timeout))
        payload = json.loads(shlex.split(command)[-1])
        assert payload["op"] == "occ.apply_changeset"
        change = payload["args"]["changes"][0]
        assert change["kind"] == "write"
        assert change["path"] == "/workspace/a.py"
        return RawExecResult(exit_code=0, stdout=json.dumps(self.response))


async def test_write_file_delegates_once_through_occ_client() -> None:
    adapter = _Adapter(
        response={
            "files": [
                {
                    "path": "/workspace/a.py",
                    "status": "committed",
                    "message": "",
                    "timings": {},
                }
            ],
            "timings": {},
        }
    )
    register_adapter("sb-write", adapter)
    try:
        result = await write_file(
            "sb-write",
            WriteFileRequest(
                path="/workspace/a.py",
                content="x",
                actor=RequestActor(agent_id="agent-1"),
            ),
        )
    finally:
        dispose_adapter("sb-write")

    assert result.success is True
    assert result.changed_paths == ("/workspace/a.py",)
    assert result.conflict is None
    assert len(adapter.calls) == 1


async def test_write_file_guard_failure_maps_conflict_info() -> None:
    adapter = _Adapter(
        response={
            "files": [
                {
                    "path": "/workspace/a.py",
                    "status": "aborted_version",
                    "message": "base_mismatch",
                    "timings": {},
                }
            ],
            "timings": {},
        }
    )
    register_adapter("sb-write-conflict", adapter)
    try:
        result = await write_file(
            "sb-write-conflict",
            WriteFileRequest(
                path="/workspace/a.py",
                content="x",
                actor=RequestActor(agent_id="agent-1"),
            ),
        )
    finally:
        dispose_adapter("sb-write-conflict")

    assert result.success is False
    assert result.status == "aborted_version"
    assert result.conflict is not None
    assert result.conflict.reason == "aborted_version"
    assert result.conflict.conflict_file == "/workspace/a.py"
    assert result.conflict.message == "base_mismatch"
    assert result.conflict_reason == "base_mismatch"
