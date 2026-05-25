"""OCC commit staging directories owned by layer-stack storage."""

from __future__ import annotations

import shutil
import tempfile
from dataclasses import dataclass
from pathlib import Path

from sandbox.layer_stack.manifest import STAGING_DIR


@dataclass(frozen=True)
class CommitStagingArea:
    staging_id: str
    path: Path


def allocate_commit_staging(
    storage_root: str | Path,
    request_id: str,
) -> CommitStagingArea:
    parent = Path(storage_root) / STAGING_DIR
    parent.mkdir(parents=True, exist_ok=True)
    path = Path(
        tempfile.mkdtemp(
            prefix=f"occ-commit-{_safe_request_part(request_id)}-",
            dir=str(parent),
        )
    )
    return CommitStagingArea(staging_id=path.name, path=path)


def drop_commit_staging(storage_root: str | Path, staging_id: str) -> None:
    if not staging_id:
        raise ValueError("staging_id must not be empty")
    shutil.rmtree(Path(storage_root) / STAGING_DIR / staging_id, ignore_errors=True)


def _safe_request_part(value: str) -> str:
    safe = "".join(ch if ch.isalnum() or ch in "-_" else "-" for ch in value)
    return safe[:48] or "request"


__all__ = [
    "CommitStagingArea",
    "allocate_commit_staging",
    "drop_commit_staging",
]
