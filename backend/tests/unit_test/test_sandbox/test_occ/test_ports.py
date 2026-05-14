"""Narrow OCC port and client contract tests."""

from __future__ import annotations

from collections.abc import Sequence

import pytest

from sandbox.occ.changeset.prepared import CommitOptions
from sandbox.occ.changeset.types import Change, ChangesetResult
from sandbox.occ.client import Client


class _FailingBindingReader:
    def require_workspace_binding(self, workspace_ref: str) -> object:
        raise RuntimeError(f"workspace is not bound: {workspace_ref}")


class _RecordingService:
    def __init__(self) -> None:
        self.called = False

    async def apply_changeset(
        self,
        changes: Sequence[Change],
        *,
        snapshot: object | None = None,
        options: CommitOptions | None = None,
    ) -> ChangesetResult:
        del changes, snapshot, options
        self.called = True
        raise AssertionError("binding check should run before OCC mutation")


async def test_occ_client_fails_closed_when_workspace_binding_is_missing() -> None:
    service = _RecordingService()
    client = Client(
        service,
        binding_reader=_FailingBindingReader(),
        workspace_ref="/tmp/missing-layer-stack",
    )

    with pytest.raises(RuntimeError, match="workspace is not bound"):
        await client.apply_changeset(())

    assert service.called is False
