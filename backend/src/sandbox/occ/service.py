"""OCC changeset preparation and commit service."""

from __future__ import annotations

import asyncio
from collections.abc import Sequence

from sandbox.layer_stack.manifest import Manifest
from sandbox.layer_stack.stack_manager import LayerStackManager
from sandbox.occ.changeset.intent import CommitIntent, PreparedChangeset
from sandbox.occ.changeset.types import Change, ChangesetResult
from sandbox.occ.commit_transaction import OccCommitTransaction
from sandbox.occ.content.gitignore_oracle import GitignoreOracle
from sandbox.occ.orchestrator import OccOrchestrator
from sandbox.occ.runtime_ops import infer_manifest_base_hash


class OccService:
    """Prepare typed OCC changesets and commit them through the layer stack."""

    def __init__(
        self,
        *,
        gitignore: GitignoreOracle,
        layer_stack: LayerStackManager | None = None,
    ) -> None:
        self._layer_stack = layer_stack
        self._orchestrator = OccOrchestrator(gitignore)
        self._transaction = (
            OccCommitTransaction(layer_stack) if layer_stack is not None else None
        )

    async def apply_changeset(
        self,
        changes: Sequence[Change],
        *,
        snapshot: Manifest | None = None,
        options: CommitIntent | None = None,
    ) -> ChangesetResult | PreparedChangeset:
        """Prepare a changeset and commit it when a layer stack is configured."""
        prepared = await self.prepare_changeset(
            changes,
            snapshot=snapshot,
            options=options,
        )
        if self._transaction is None:
            return prepared
        return await asyncio.to_thread(
            self._transaction.revalidate_and_publish,
            prepared,
        )

    async def prepare_changeset(
        self,
        changes: Sequence[Change],
        *,
        snapshot: Manifest | None = None,
        options: CommitIntent | None = None,
    ) -> PreparedChangeset:
        """Route changes and infer leased-snapshot base hashes."""
        intent = options or CommitIntent()
        base_hash_reader = None
        if snapshot is not None and self._layer_stack is not None:
            layer_stack = self._layer_stack

            def base_hash_reader(path: str) -> str | None:
                return infer_manifest_base_hash(
                    layer_stack=layer_stack,
                    manifest=snapshot,
                    path=path,
                )

        return await self._orchestrator.prepare(
            changes,
            snapshot=snapshot,
            intent=intent,
            base_hash_reader=base_hash_reader,
        )


__all__ = ["OccService"]
