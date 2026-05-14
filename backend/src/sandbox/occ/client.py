"""Narrow OCC mutation client boundary."""

from __future__ import annotations

from collections.abc import Sequence
from typing import Protocol

from sandbox.layer_stack.manifest import Manifest
from sandbox.occ.changeset.prepared import CommitOptions, PreparedChangeset
from sandbox.occ.changeset.types import Change, ChangesetResult
from sandbox.occ.ports import WorkspaceBindingReader


class MutationService(Protocol):
    async def prepare_changeset(
        self,
        changes: Sequence[Change],
        *,
        snapshot: Manifest | None = None,
        options: CommitOptions | None = None,
    ) -> PreparedChangeset: ...

    async def apply_changeset(
        self,
        changes: Sequence[Change],
        *,
        snapshot: Manifest | None = None,
        options: CommitOptions | None = None,
    ) -> ChangesetResult: ...

    async def commit_prepared(
        self,
        prepared: PreparedChangeset,
    ) -> ChangesetResult: ...

    def prepare_changeset_sync(
        self,
        changes: Sequence[Change],
        *,
        snapshot: Manifest | None = None,
        options: CommitOptions | None = None,
    ) -> PreparedChangeset: ...

    def apply_changeset_sync(
        self,
        changes: Sequence[Change],
        *,
        snapshot: Manifest | None = None,
        options: CommitOptions | None = None,
    ) -> ChangesetResult: ...

    def commit_prepared_sync(
        self,
        prepared: PreparedChangeset,
    ) -> ChangesetResult: ...


class Client:
    """Command-exec-facing client for submitting typed mutation changesets."""

    def __init__(
        self,
        service: MutationService,
        *,
        binding_reader: WorkspaceBindingReader,
        workspace_ref: str = "",
    ) -> None:
        self._service = service
        self._binding_reader = binding_reader
        self._workspace_ref = workspace_ref

    def _require_binding(self, workspace_ref: str | None) -> None:
        ref = self._workspace_ref if workspace_ref is None else workspace_ref
        self._binding_reader.require_workspace_binding(ref)

    async def prepare_changeset(
        self,
        typed_changes: Sequence[Change],
        *,
        snapshot: Manifest | None = None,
        options: CommitOptions | None = None,
        workspace_ref: str | None = None,
    ) -> PreparedChangeset:
        self._require_binding(workspace_ref)
        return await self._service.prepare_changeset(
            typed_changes,
            snapshot=snapshot,
            options=options,
        )

    async def apply_changeset(
        self,
        typed_changes: Sequence[Change],
        *,
        snapshot: Manifest | None = None,
        options: CommitOptions | None = None,
        workspace_ref: str | None = None,
    ) -> ChangesetResult:
        self._require_binding(workspace_ref)
        return await self._service.apply_changeset(
            typed_changes,
            snapshot=snapshot,
            options=options,
        )

    async def commit_prepared(
        self,
        prepared: PreparedChangeset,
        *,
        workspace_ref: str | None = None,
    ) -> ChangesetResult:
        """Commit a caller-prepared changeset after the standard binding check."""
        self._require_binding(workspace_ref)
        return await self._service.commit_prepared(prepared)

    def prepare_changeset_sync(
        self,
        typed_changes: Sequence[Change],
        *,
        snapshot: Manifest | None = None,
        options: CommitOptions | None = None,
        workspace_ref: str | None = None,
    ) -> PreparedChangeset:
        self._require_binding(workspace_ref)
        return self._service.prepare_changeset_sync(
            typed_changes,
            snapshot=snapshot,
            options=options,
        )

    def apply_changeset_sync(
        self,
        typed_changes: Sequence[Change],
        *,
        snapshot: Manifest | None = None,
        options: CommitOptions | None = None,
        workspace_ref: str | None = None,
    ) -> ChangesetResult:
        self._require_binding(workspace_ref)
        return self._service.apply_changeset_sync(
            typed_changes,
            snapshot=snapshot,
            options=options,
        )

    def commit_prepared_sync(
        self,
        prepared: PreparedChangeset,
        *,
        workspace_ref: str | None = None,
    ) -> ChangesetResult:
        self._require_binding(workspace_ref)
        return self._service.commit_prepared_sync(prepared)


__all__ = ["MutationService", "Client"]
