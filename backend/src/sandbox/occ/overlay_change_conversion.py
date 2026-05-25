"""Convert overlay/workspace captures into OCC changes."""

from __future__ import annotations

import os
from collections.abc import Sequence
from typing import TYPE_CHECKING

from sandbox.occ.changeset import (
    build_overlay_delete_change,
    build_overlay_write_change,
)
from sandbox.occ.changeset import Change, ChangeSource, OpaqueDirChange, SymlinkChange

if TYPE_CHECKING:
    from sandbox.overlay.path_change import OverlayPathChange


def overlay_path_changes_to_occ_changes(
    path_changes: Sequence[OverlayPathChange],
    *,
    source: ChangeSource = ChangeSource.OVERLAY_CAPTURE,
) -> tuple[Change, ...]:
    """Convert policy-blind path changes into typed OCC mutations.

    ``write`` kinds thread ``content_path`` and ``final_hash`` (both
    already populated during overlay capture) into the ``WriteChange``
    instead of reading bytes here — the OCC stager copies the file
    in-kernel and reuses the precomputed hash.
    """
    changes: list[Change] = []
    for path_change in path_changes:
        if path_change.kind == "write":
            if path_change.content_path is None:
                raise ValueError(f"write workspace change lacks content path: {path_change.path}")
            if path_change.final_hash is None:
                raise ValueError(f"write workspace change lacks final_hash: {path_change.path}")
            changes.append(
                build_overlay_write_change(
                    path=path_change.path,
                    content_path=path_change.content_path,
                    precomputed_hash=path_change.final_hash,
                    source=source,
                )
            )
            continue
        if path_change.kind == "delete":
            changes.append(build_overlay_delete_change(path=path_change.path, source=source))
            continue
        if path_change.kind == "symlink":
            if path_change.content_path is None:
                raise ValueError(f"symlink workspace change lacks content path: {path_change.path}")
            changes.append(
                SymlinkChange(
                    path=path_change.path,
                    target=os.readlink(path_change.content_path),
                    source=source,
                )
            )
            continue
        if path_change.kind == "opaque_dir":
            changes.append(
                OpaqueDirChange(
                    path=path_change.path,
                    source=source,
                )
            )
            continue
        raise ValueError(
            f"unsupported overlay path change kind for {path_change.path}: {path_change.kind!r}"
        )
    return tuple(changes)


__all__ = [
    "overlay_path_changes_to_occ_changes",
]
