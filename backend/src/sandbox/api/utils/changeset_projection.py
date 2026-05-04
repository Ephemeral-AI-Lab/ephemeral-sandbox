"""Internal helpers projecting a :class:`ChangesetResult` onto guarded API results.

Used by :mod:`sandbox.api.write` and :mod:`sandbox.api.edit`. Not part of the
public API surface — both verbs translate the gate's per-file
``FileResult`` outcomes into the existing ``WriteFileResult`` /
``EditFileResult`` shapes via these helpers so the duplication stays out of
the verb modules.
"""

from __future__ import annotations

from collections.abc import Sequence

from sandbox.api.utils.models import ConflictInfo
from sandbox.occ.changeset.types import (
    FileResult,
    is_published_status,
    is_success_status,
)


def committed_paths(
    files: Sequence[FileResult],
    *,
    fallback_path: str,
) -> tuple[str, ...]:
    """Return paths of every COMMITTED ``FileResult``, or a single-path fallback."""
    committed = tuple(f.path for f in files if is_published_status(f.status) and f.path)
    if committed:
        return committed
    aborted = next(
        (f for f in files if not is_published_status(f.status) and f.path),
        None,
    )
    if aborted is not None:
        return (aborted.path,)
    return (fallback_path,) if not files else ()


def conflict_and_status(
    files: Sequence[FileResult],
) -> tuple[ConflictInfo | None, str]:
    """Surface the first non-COMMITTED ``FileResult`` as a ``ConflictInfo`` + status."""
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

__all__ = ["committed_paths", "conflict_and_status"]
