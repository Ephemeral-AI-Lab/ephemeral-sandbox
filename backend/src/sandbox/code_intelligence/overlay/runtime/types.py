"""Shared types for the sandbox-side overlay runtime."""

from __future__ import annotations

import os
from dataclasses import dataclass


@dataclass(frozen=True)
class UpperEntry:
    """One upperdir entry handed to the classifier."""

    rel: str
    st: os.stat_result
    xattrs: dict[bytes, bytes]
    upper_path: str


@dataclass(frozen=True)
class GitincludeChange:
    """One gitinclude-route change to emit to NDJSON for OCC."""

    path: str
    kind: str
    base_content: str
    base_existed: bool
    final_content: str | None


@dataclass(frozen=True)
class ClassifyOutcome:
    gitinclude: tuple[GitincludeChange, ...]
    gitignore_paths: tuple[str, ...]
    direct_merged_bytes: int
    whiteouts_gitinclude: int
    whiteouts_gitignore_refused: int
    dotgit_rejects: int


@dataclass(frozen=True)
class PolicyRejectOutcome:
    reason: str
    paths: tuple[str, ...]


@dataclass(frozen=True)
class DirectMergeOp:
    rel: str
    upper_path: str
    st: os.stat_result


@dataclass(frozen=True)
class OpaquePruneOp:
    rel: str
    upper_path: str


@dataclass(frozen=True)
class ClassificationPlan:
    gitinclude: tuple[GitincludeChange, ...]
    gitignore_paths: tuple[str, ...]
    direct_merge_ops: tuple[DirectMergeOp, ...]
    opaque_prune_ops: tuple[OpaquePruneOp, ...]
    whiteouts_gitinclude: int
    whiteouts_gitignore_refused: int
    dotgit_rejects: int

    def to_outcome(self, *, direct_merged_bytes: int) -> ClassifyOutcome:
        return ClassifyOutcome(
            gitinclude=self.gitinclude,
            gitignore_paths=self.gitignore_paths,
            direct_merged_bytes=direct_merged_bytes,
            whiteouts_gitinclude=self.whiteouts_gitinclude,
            whiteouts_gitignore_refused=self.whiteouts_gitignore_refused,
            dotgit_rejects=self.dotgit_rejects,
        )


class ClassifierIOError(RuntimeError):
    """Raised when the classifier cannot read an expected upperdir file."""


__all__ = [
    "ClassificationPlan",
    "ClassifierIOError",
    "ClassifyOutcome",
    "DirectMergeOp",
    "GitincludeChange",
    "OpaquePruneOp",
    "PolicyRejectOutcome",
    "UpperEntry",
]
