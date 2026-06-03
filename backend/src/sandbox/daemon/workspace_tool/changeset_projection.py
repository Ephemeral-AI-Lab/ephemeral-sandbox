"""Project changeset results onto guarded operation result shapes."""

from __future__ import annotations

from collections.abc import Sequence

from sandbox._shared.models import ConflictInfo
from sandbox.occ.changeset import (
    FileResult,
    is_published_status,
    is_success_status,
)
from sandbox._shared.timing_keys import TimingKey


def published_paths(files: Sequence[FileResult]) -> tuple[str, ...]:
    """Return paths of every published ``FileResult``."""
    return tuple(f.path for f in files if is_published_status(f.status) and f.path)


def conflict_and_status(
    files: Sequence[FileResult],
) -> tuple[ConflictInfo | None, str]:
    """Surface the first non-COMMITTED ``FileResult`` as a conflict + status."""
    if not files:
        return None, "committed"
    bad = next((f for f in files if not is_success_status(f.status)), None)
    if bad is None:
        return None, "committed"
    status = bad.status.value
    return (
        ConflictInfo(
            reason=status,
            conflict_file=bad.path or None,
            message=bad.message or status,
        ),
        status,
    )


def conflict_to_dict(conflict: object | None) -> dict[str, object] | None:
    """Serialize a conflict object into the public guarded-result shape."""
    if conflict is None:
        return None
    return {
        "reason": getattr(conflict, "reason", ""),
        "conflict_file": getattr(conflict, "conflict_file", None),
        "message": getattr(conflict, "message", ""),
    }


def gitignore_cache_timings(gitignore: object) -> dict[str, float]:
    """Expose gitignore-oracle cache counters as result timing metrics."""
    # WR-01: default the counters; only SnapshotGitignoreOracle exposes
    # them. A test mock or alternative oracle satisfying the protocol
    # without these counters used to crash here at result-shape time.
    return {
        TimingKey.GITIGNORE_CACHE_HITS_TOTAL: float(getattr(gitignore, "cache_hits", 0)),
        TimingKey.GITIGNORE_CACHE_MISSES_TOTAL: float(getattr(gitignore, "cache_misses", 0)),
    }


__all__ = [
    "conflict_and_status",
    "conflict_to_dict",
    "gitignore_cache_timings",
    "published_paths",
]
