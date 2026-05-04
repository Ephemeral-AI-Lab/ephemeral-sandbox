"""Tests for ``sandbox.api.edit``."""

from __future__ import annotations

from sandbox.api.edit import edit_file
from sandbox.api.utils.models import (
    EditFileRequest,
    RequestActor,
    SearchReplaceEdit,
)
from sandbox.occ.changeset.types import ChangesetResult, FileResult, FileStatus
from sandbox.occ.client import dispose_occ_service, register_occ_service


class _Service:
    name = "edit-api"

    def __init__(self, *, response: ChangesetResult) -> None:
        self.response = response
        self.calls: list[tuple[tuple[object, ...], object]] = []

    async def apply_changeset(self, changes, *, snapshot=None, options=None):
        del snapshot
        self.calls.append((tuple(changes), options))
        return self.response


async def test_edit_file_delegates_once_and_counts_applied_edits() -> None:
    service = _Service(
        response=ChangesetResult(
            files=(FileResult(path="/workspace/a.py", status=FileStatus.COMMITTED),)
        )
    )
    register_occ_service("sb-edit", service)
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
        dispose_occ_service("sb-edit")

    assert result.success is True
    assert result.changed_paths == ("/workspace/a.py",)
    assert result.applied_edits == 1
    assert len(service.calls) == 1
    change = service.calls[0][0][0]
    assert change.path == "/workspace/a.py"
    assert change.old_text == "old"


async def test_edit_file_guard_failure_maps_conflict_info() -> None:
    service = _Service(
        response=ChangesetResult(
            files=(
                FileResult(
                    path="/workspace/a.py",
                    status=FileStatus.ABORTED_OVERLAP,
                    message="patch_failed",
                ),
            )
        )
    )
    register_occ_service("sb-edit-conflict", service)
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
        dispose_occ_service("sb-edit-conflict")

    assert result.success is False
    assert result.applied_edits == 0
    assert result.status == "aborted_overlap"
    assert result.conflict is not None
    assert result.conflict.reason == "aborted_overlap"
    assert result.conflict.message == "patch_failed"
    assert result.conflict_reason == "patch_failed"
