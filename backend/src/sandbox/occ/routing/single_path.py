"""Fast OCC preparation for public single-path file operations."""

from __future__ import annotations

import time
from typing import Protocol

from sandbox.layer_stack.layer.change import normalize_layer_path
from sandbox.layer_stack.manifest import Manifest
from sandbox.occ.changeset.prepared import (
    PreparedChangeset,
    PreparedPathGroup,
    RouteDecision,
)
from sandbox.occ.changeset.types import Change
from sandbox.occ.routing.orchestrator import (
    BaseHashReader,
    attach_base_hash,
    requires_base_hash,
)


class SnapshotIgnoreOracle(Protocol):
    def is_ignored_in_snapshot(self, path: str, snapshot: Manifest) -> bool: ...


def prepare_single_path_changeset(
    change: Change,
    *,
    snapshot: Manifest,
    gitignore: SnapshotIgnoreOracle,
    base_hash_reader: BaseHashReader | None = None,
    atomic: bool = False,
) -> PreparedChangeset:
    """Prepare one path without materializing a full gitignore workspace."""
    total_start = time.perf_counter()
    timings: dict[str, float] = {}
    route_start = time.perf_counter()

    try:
        path = normalize_layer_path(change.path)
    except ValueError as exc:
        group = PreparedPathGroup(
            path=str(change.path),
            route=RouteDecision.REJECT,
            changes=(change,),
            message=str(exc),
        )
        timings["occ.prepare.route_and_base_hash_s"] = time.perf_counter() - route_start
        timings["occ.prepare.single_path_fast_s"] = timings[
            "occ.prepare.route_and_base_hash_s"
        ]
        timings["occ.prepare.total_s"] = time.perf_counter() - total_start
        return PreparedChangeset(
            snapshot=snapshot,
            path_groups=(group,),
            atomic=atomic,
            timings=timings,
        )

    route, message = _route_single_path(
        path,
        snapshot=snapshot,
        gitignore=gitignore,
        timings=timings,
    )
    prepared_change = change
    base_hash = None
    if route is RouteDecision.OCC_GATED_MERGE and requires_base_hash(change):
        base_hash_start = time.perf_counter()
        base_hash = base_hash_reader(path) if base_hash_reader is not None else None
        timings["occ.prepare.single_path_base_hash_s"] = (
            time.perf_counter() - base_hash_start
        )
        prepared_change = attach_base_hash(change, base_hash)
    else:
        timings["occ.prepare.single_path_base_hash_s"] = 0.0

    group = PreparedPathGroup(
        path=path,
        route=route,
        changes=(prepared_change,),
        base_hash=base_hash,
        message=message,
    )
    timings["occ.prepare.route_and_base_hash_s"] = time.perf_counter() - route_start
    timings["occ.prepare.single_path_fast_s"] = timings[
        "occ.prepare.route_and_base_hash_s"
    ]
    timings["occ.prepare.total_s"] = time.perf_counter() - total_start
    return PreparedChangeset(
        snapshot=snapshot,
        path_groups=(group,),
        atomic=atomic,
        timings=timings,
    )


def _route_single_path(
    path: str,
    *,
    snapshot: Manifest,
    gitignore: SnapshotIgnoreOracle,
    timings: dict[str, float],
) -> tuple[RouteDecision, str | None]:
    if path == ".git" or path.startswith(".git/"):
        timings["occ.prepare.gitignore_s"] = 0.0
        return RouteDecision.DROP, ".git paths are not mutable through OCC"

    gitignore_start = time.perf_counter()
    ignored = gitignore.is_ignored_in_snapshot(path, snapshot)
    timings["occ.prepare.gitignore_s"] = time.perf_counter() - gitignore_start
    if ignored:
        return RouteDecision.OCC_SKIPPED_MERGE, None
    return RouteDecision.OCC_GATED_MERGE, None


__all__ = ["BaseHashReader", "SnapshotIgnoreOracle", "prepare_single_path_changeset"]
