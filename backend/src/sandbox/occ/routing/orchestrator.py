"""Route OCC changes into direct or gated prepared path groups."""

from __future__ import annotations

from collections import OrderedDict
from collections.abc import Callable, Sequence

from sandbox.layer_stack.layer.change import normalize_layer_path
from sandbox.layer_stack.manifest import Manifest
from sandbox.occ.changeset.prepared import (
    CommitOptions,
    PreparedChangeset,
    PreparedPathGroup,
    RouteDecision,
)
from sandbox.occ.changeset.types import (
    Change,
    DeleteChange,
    WriteChange,
)
from sandbox.occ.content.gitignore_oracle import (
    GitignoreMatcher,
    SnapshotGitignoreMatcher,
)
from sandbox.occ.content.hashing import ContentHasher
from sandbox.timing import monotonic_now

BaseHashReader = Callable[[str], str | None]


class Router:
    """Prepare direct and gated path groups for a typed changeset."""

    def __init__(self, gitignore: GitignoreMatcher) -> None:
        self._gitignore = gitignore

    def prepare_sync(
        self,
        changes: Sequence[Change],
        *,
        snapshot: Manifest | None,
        options: CommitOptions,
        base_hash_reader: BaseHashReader | None = None,
    ) -> PreparedChangeset:
        """Route changes and infer gated base hashes synchronously by path."""
        group_start = monotonic_now()
        grouped = self._group_by_route(changes, snapshot=snapshot)
        groups_end = monotonic_now()
        prepared = tuple(
            self._prepare_group(
                path,
                route,
                tuple(path_changes),
                message,
                base_hash_reader,
            )
            for path, route, path_changes, message in grouped
        )
        prepare_end = monotonic_now()
        return PreparedChangeset(
            snapshot=snapshot,
            path_groups=prepared,
            atomic=options.atomic,
            timings={
                "occ.prepare.group_by_route_s": groups_end - group_start,
                "occ.prepare.prepare_groups_s": prepare_end - groups_end,
            },
        )

    def prepare_single_path_sync(
        self,
        change: Change,
        *,
        snapshot: Manifest,
        base_hash_reader: BaseHashReader | None = None,
        atomic: bool = False,
    ) -> PreparedChangeset:
        """Prepare one path through the same routing rules as batch prepare."""
        total_start = monotonic_now()
        route_start = monotonic_now()
        route, path, message = self._route_change(change, snapshot=snapshot)
        prepared_change = change
        timings: dict[str, float] = {}
        if route is RouteDecision.GATED and requires_base_hash(change):
            base_hash_start = monotonic_now()
            base_hash = base_hash_reader(path) if base_hash_reader is not None else None
            timings["occ.prepare.single_path_base_hash_s"] = (
                monotonic_now() - base_hash_start
            )
            prepared_change = attach_base_hash(change, base_hash)
        else:
            timings["occ.prepare.single_path_base_hash_s"] = 0.0

        group = PreparedPathGroup(
            path=path,
            route=route,
            changes=(prepared_change,),
            message=message,
        )
        timings["occ.prepare.route_and_base_hash_s"] = monotonic_now() - route_start
        timings["occ.prepare.single_path_fast_s"] = timings[
            "occ.prepare.route_and_base_hash_s"
        ]
        timings["occ.prepare.total_s"] = monotonic_now() - total_start
        return PreparedChangeset(
            snapshot=snapshot,
            path_groups=(group,),
            atomic=atomic,
            timings=timings,
        )

    def _group_by_route(
        self,
        changes: Sequence[Change],
        *,
        snapshot: Manifest | None,
    ) -> list[tuple[str, RouteDecision, list[Change], str | None]]:
        grouped: OrderedDict[
            tuple[RouteDecision, str], tuple[list[Change], str | None]
        ] = OrderedDict()
        for change in changes:
            route, path, message = self._route_change(change, snapshot=snapshot)
            key = (route, path)
            if key not in grouped:
                grouped[key] = ([], message)
            grouped[key][0].append(change)
        return [
            (path, route, path_changes, message)
            for (route, path), (path_changes, message) in grouped.items()
        ]

    def _route_change(
        self,
        change: Change,
        *,
        snapshot: Manifest | None,
    ) -> tuple[RouteDecision, str, str | None]:
        try:
            path = normalize_layer_path(change.path)
        except ValueError as exc:
            return RouteDecision.REJECT, str(change.path), str(exc)

        if path == ".git" or path.startswith(".git/"):
            return RouteDecision.DROP, path, ".git paths are not mutable through OCC"

        if _is_gitignored(self._gitignore, path=path, snapshot=snapshot):
            return RouteDecision.DIRECT, path, None
        return RouteDecision.GATED, path, None

    def _prepare_group(
        self,
        path: str,
        route: RouteDecision,
        changes: tuple[Change, ...],
        message: str | None,
        base_hash_reader: BaseHashReader | None,
    ) -> PreparedPathGroup:
        if route is not RouteDecision.GATED or base_hash_reader is None:
            return PreparedPathGroup(
                path=path,
                route=route,
                changes=changes,
                message=message,
            )

        prepared_changes = _attach_chained_base_hashes(
            path,
            changes,
            base_hash_reader,
        )
        return PreparedPathGroup(
            path=path,
            route=route,
            changes=prepared_changes,
            message=message,
        )


def requires_base_hash(change: Change) -> bool:
    return (
        isinstance(change, (WriteChange, DeleteChange))
        and change.base_hash is None
        and change.source in ("api_write", "overlay_capture")
    )


def attach_base_hash(change: Change, base_hash: str | None) -> Change:
    if isinstance(change, WriteChange):
        return change.with_base_hash(base_hash)
    if isinstance(change, DeleteChange):
        return change.with_base_hash(base_hash)
    return change


def _attach_chained_base_hashes(
    path: str,
    changes: tuple[Change, ...],
    base_hash_reader: BaseHashReader,
) -> tuple[Change, ...]:
    needs_base_hash = any(requires_base_hash(change) for change in changes)
    running_hash = base_hash_reader(path) if needs_base_hash else None
    hasher = ContentHasher()
    prepared: list[Change] = []
    for change in changes:
        next_change = (
            attach_base_hash(change, running_hash)
            if requires_base_hash(change)
            else change
        )
        prepared.append(next_change)
        if isinstance(change, WriteChange):
            running_hash = change.precomputed_hash or hasher.hash_bytes(
                change.final_content
            )
        elif isinstance(change, DeleteChange):
            running_hash = None
    return tuple(prepared)


def _is_gitignored(
    oracle: GitignoreMatcher,
    *,
    path: str,
    snapshot: Manifest | None,
) -> bool:
    if snapshot is not None:
        if not isinstance(oracle, SnapshotGitignoreMatcher):
            raise TypeError(
                "snapshot-aware OCC routing requires "
                "SnapshotGitignoreMatcher.is_ignored_in_snapshot"
            )
        return oracle.is_ignored_in_snapshot(path, snapshot)
    return oracle.is_ignored(path)


OccOrchestrator = Router


__all__ = [
    "BaseHashReader",
    "OccOrchestrator",
    "Router",
    "attach_base_hash",
    "requires_base_hash",
]
