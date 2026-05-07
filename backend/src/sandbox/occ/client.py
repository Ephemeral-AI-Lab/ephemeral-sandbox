"""Narrow OCC mutation client boundary."""

from __future__ import annotations

from collections.abc import Sequence
from typing import Protocol

from sandbox.layer_stack.manifest import Manifest
from sandbox.occ.changeset.prepared import CommitOptions, PreparedChangeset
from sandbox.occ.changeset.types import Change, ChangesetResult
from sandbox.occ.ports import WorkspaceBindingReader


class OCCMutationService(Protocol):
    async def apply_changeset(
        self,
        changes: Sequence[Change],
        *,
        snapshot: Manifest | None = None,
        options: CommitOptions | None = None,
    ) -> ChangesetResult | PreparedChangeset: ...


class OCCClient:
    """Command-exec-facing client for submitting typed mutation changesets."""

    def __init__(
        self,
        service: OCCMutationService,
        *,
        binding_reader: WorkspaceBindingReader,
        workspace_ref: str = "",
    ) -> None:
        self._service = service
        self._binding_reader = binding_reader
        self._workspace_ref = workspace_ref

    async def apply_changeset(
        self,
        typed_changes: Sequence[Change],
        *,
        snapshot: Manifest | None = None,
        options: CommitOptions | None = None,
        workspace_ref: str | None = None,
    ) -> ChangesetResult | PreparedChangeset:
        ref = self._workspace_ref if workspace_ref is None else workspace_ref
        self._binding_reader.require_workspace_binding(ref)
        return await self._service.apply_changeset(
            typed_changes,
            snapshot=snapshot,
            options=options,
        )


__all__ = ["OCCClient", "OCCMutationService"]
