"""Tests for ``sandbox.api.edit``."""

from __future__ import annotations

import json
import shlex

from sandbox.api.edit import edit_file
from sandbox.api.models import (
    EditFileRequest,
    RawExecResult,
    RequestActor,
    SearchReplaceEdit,
)
from sandbox.providers.registry import dispose_adapter, register_adapter


class _Adapter:
    name = "edit-api"

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
        assert change["kind"] == "edit"
        assert change["path"] == "/workspace/a.py"
        assert change["edits"][0]["old_text"] == "old"
        return RawExecResult(exit_code=0, stdout=json.dumps(self.response))


async def test_edit_file_delegates_once_and_counts_applied_edits() -> None:
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
    register_adapter("sb-edit", adapter)
    try:
        result = await edit_file(
            "sb-edit",
            EditFileRequest(
                path="/workspace/a.py",
                edits=(SearchReplaceEdit(old_text="old", new_text="new"),),
                actor=RequestActor(agent_id="agent-1"),
            ),
        )
    finally:
        dispose_adapter("sb-edit")

    assert result.success is True
    assert result.changed_paths == ("/workspace/a.py",)
    assert result.applied_edits == 1
    assert len(adapter.calls) == 1


async def test_edit_file_guard_failure_maps_conflict_info() -> None:
    adapter = _Adapter(
        response={
            "files": [
                {
                    "path": "/workspace/a.py",
                    "status": "aborted_overlap",
                    "message": "patch_failed",
                    "timings": {},
                }
            ],
            "timings": {},
        }
    )
    register_adapter("sb-edit-conflict", adapter)
    try:
        result = await edit_file(
            "sb-edit-conflict",
            EditFileRequest(
                path="/workspace/a.py",
                edits=(SearchReplaceEdit(old_text="old", new_text="new"),),
                actor=RequestActor(agent_id="agent-1"),
            ),
        )
    finally:
        dispose_adapter("sb-edit-conflict")

    assert result.success is False
    assert result.applied_edits == 0
    assert result.status == "aborted_overlap"
    assert result.conflict is not None
    assert result.conflict.reason == "aborted_overlap"
    assert result.conflict.message == "patch_failed"
    assert result.conflict_reason == "patch_failed"
