"""Runtime helpers and typed ``occ.apply_changeset`` dispatch."""

from __future__ import annotations

from collections.abc import Sequence
from typing import Protocol

from sandbox.layer_stack.manifest import Manifest
from sandbox.layer_stack.stack_manager import LayerStackManager
from sandbox.occ.changeset.intent import CommitIntent, PreparedChangeset
from sandbox.occ.changeset.types import Change, ChangesetResult
from sandbox.occ.content.hashing import ContentHasher


class ApplyChangesetService(Protocol):
    async def apply_changeset(
        self,
        changes: Sequence[Change],
        *,
        snapshot: Manifest | None = None,
        options: CommitIntent | None = None,
    ) -> ChangesetResult | PreparedChangeset: ...


def content_hash_bytes(content: bytes) -> str:
    """Return the layer-stack OCC hash for file bytes."""
    return ContentHasher().hash_bytes(content)


async def apply_changeset_op(
    service: ApplyChangesetService,
    changes: Sequence[Change],
    *,
    snapshot: Manifest | None = None,
    intent: CommitIntent | None = None,
) -> ChangesetResult | PreparedChangeset:
    """Dispatch a typed OCC apply operation to the configured service."""
    return await service.apply_changeset(
        changes,
        snapshot=snapshot,
        options=intent,
    )


def infer_manifest_base_hash(
    *,
    layer_stack: LayerStackManager,
    manifest: Manifest,
    path: str,
) -> str | None:
    """Hash *path* content as it existed in a leased manifest."""
    content, exists = layer_stack.read_bytes(path, manifest)
    if not exists or content is None:
        return None
    return content_hash_bytes(content)


__all__ = [
    "ApplyChangesetService",
    "apply_changeset_op",
    "content_hash_bytes",
    "infer_manifest_base_hash",
]
