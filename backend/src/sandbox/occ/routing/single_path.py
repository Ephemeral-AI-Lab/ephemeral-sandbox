"""Compatibility wrapper for single-path OCC preparation."""

from __future__ import annotations

from sandbox.layer_stack.manifest import Manifest
from sandbox.occ.changeset.prepared import PreparedChangeset
from sandbox.occ.changeset.types import Change
from sandbox.occ.content.gitignore_oracle import SnapshotGitignoreMatcher
from sandbox.occ.routing.orchestrator import BaseHashReader, Router


def prepare_single_path_changeset(
    change: Change,
    *,
    snapshot: Manifest,
    gitignore: SnapshotGitignoreMatcher,
    base_hash_reader: BaseHashReader | None = None,
    atomic: bool = False,
) -> PreparedChangeset:
    """Prepare one path through the shared router fast branch."""
    return Router(gitignore).prepare_single_path_sync(
        change,
        snapshot=snapshot,
        base_hash_reader=base_hash_reader,
        atomic=atomic,
    )


SnapshotIgnoreOracle = SnapshotGitignoreMatcher


__all__ = [
    "BaseHashReader",
    "SnapshotIgnoreOracle",
    "prepare_single_path_changeset",
]
