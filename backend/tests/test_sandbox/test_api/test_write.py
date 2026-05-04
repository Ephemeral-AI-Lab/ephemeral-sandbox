"""Tests for ``sandbox.api.write``."""

from __future__ import annotations

from sandbox.api.utils.models import RequestActor, WriteFileRequest
from sandbox.api.write import write_file
from sandbox.occ.changeset.types import ChangesetResult, FileResult, FileStatus
from sandbox.occ.client import dispose_occ_service, register_occ_service


class _Service:
    name = "write-api"

    def __init__(self, *, response: ChangesetResult) -> None:
        self.response = response
        self.calls: list[tuple[tuple[object, ...], object]] = []

    async def apply_changeset(self, changes, *, snapshot=None, options=None):
        del snapshot
        self.calls.append((tuple(changes), options))
        return self.response


async def test_write_file_delegates_once_through_occ_client() -> None:
    service = _Service(
        response=ChangesetResult(
            files=(FileResult(path="/workspace/a.py", status=FileStatus.COMMITTED),)
        )
    )
    register_occ_service("sb-write", service)
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
        dispose_occ_service("sb-write")

    assert result.success is True
    assert result.changed_paths == ("/workspace/a.py",)
    assert result.conflict is None
    assert len(service.calls) == 1
    change = service.calls[0][0][0]
    assert change.path == "/workspace/a.py"
    assert change.final_content == b"x"


async def test_write_file_guard_failure_maps_conflict_info() -> None:
    service = _Service(
        response=ChangesetResult(
            files=(
                FileResult(
                    path="/workspace/a.py",
                    status=FileStatus.ABORTED_VERSION,
                    message="base_mismatch",
                ),
            )
        )
    )
    register_occ_service("sb-write-conflict", service)
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
        dispose_occ_service("sb-write-conflict")

    assert result.success is False
    assert result.status == "aborted_version"
    assert result.conflict is not None
    assert result.conflict.reason == "aborted_version"
    assert result.conflict.conflict_file == "/workspace/a.py"
    assert result.conflict.message == "base_mismatch"
    assert result.conflict_reason == "base_mismatch"
