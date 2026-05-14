"""Convert overlay/workspace captures into OCC changes."""

from __future__ import annotations

import os
from collections.abc import Sequence

from sandbox.layer_stack.layer_change import normalize_layer_path
from sandbox.occ.changeset.types import (
    build_overlay_delete_change,
    build_overlay_write_change,
)
from sandbox.occ.changeset.types import Change, OpaqueDirChange, SymlinkChange
from sandbox.execution.overlay.change import OverlayPathChange


def overlay_path_changes_to_occ_changes(
    path_changes: Sequence[OverlayPathChange],
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
                )
            )
            continue
        if path_change.kind == "delete":
            changes.append(build_overlay_delete_change(path=path_change.path))
            continue
        if path_change.kind == "symlink":
            if path_change.content_path is None:
                raise ValueError(f"symlink workspace change lacks content path: {path_change.path}")
            changes.append(
                SymlinkChange(
                    path=path_change.path,
                    target=os.readlink(path_change.content_path),
                    source="overlay_capture",
                )
            )
            continue
        if path_change.kind == "opaque_dir":
            changes.append(
                OpaqueDirChange(
                    path=path_change.path,
                    kept_children=frozenset(_kept_children_for(path_change.path, path_changes)),
                    source="overlay_capture",
                )
            )
            continue
    return tuple(changes)


def _kept_children_for(
    rel: str,
    path_changes: Sequence[OverlayPathChange],
) -> set[str]:
    rel_norm = normalize_layer_path(rel, allow_root=True)
    prefix = f"{rel_norm}/" if rel_norm else ""
    kept: set[str] = set()
    for item in path_changes:
        item_path = normalize_layer_path(item.path, allow_root=True)
        if item_path == rel_norm or not item_path.startswith(prefix):
            continue
        rest = item_path[len(prefix) :]
        if rest:
            kept.add(rest.split("/", 1)[0])
    return kept


__all__ = [
    "overlay_path_changes_to_occ_changes",
]
