"""Route OCC changes into direct or gated prepared path groups."""

from __future__ import annotations

from collections import OrderedDict
from collections.abc import Callable, Sequence

from sandbox.layer_stack.layer_change import normalize_layer_path
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
from sandbox.occ.timing_keys import TimingKey
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
                TimingKey.PREPARE_GROUP_BY_ROUTE: groups_end - group_start,
                TimingKey.PREPARE_GROUPS: prepare_end - groups_end,
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
        route, path, message, gitignore_s = self._route_change_timed(
            change,
            snapshot=snapshot,
        )
        prepared_change = change
        timings: dict[str, float] = {TimingKey.PREPARE_GITIGNORE: gitignore_s}
        if route is RouteDecision.GATED and _requires_base_hash(change):
            base_hash_start = monotonic_now()
            base_hash = base_hash_reader(path) if base_hash_reader is not None else None
            timings[TimingKey.PREPARE_SINGLE_PATH_BASE_HASH] = monotonic_now() - base_hash_start
            prepared_change = _attach_base_hash(change, base_hash)
        else:
            timings[TimingKey.PREPARE_SINGLE_PATH_BASE_HASH] = 0.0

        group = PreparedPathGroup(
            path=path,
            route=route,
            changes=(prepared_change,),
            message=message,
        )
        timings[TimingKey.PREPARE_ROUTE_AND_BASE_HASH] = monotonic_now() - route_start
        timings[TimingKey.PREPARE_SINGLE_PATH_FAST] = timings[TimingKey.PREPARE_ROUTE_AND_BASE_HASH]
        timings[TimingKey.PREPARE_TOTAL] = monotonic_now() - total_start
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
        grouped: OrderedDict[tuple[RouteDecision, str], tuple[list[Change], str | None]] = (
            OrderedDict()
        )
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
        route, path, message, _gitignore_s = self._route_change_timed(
            change,
            snapshot=snapshot,
        )
        return route, path, message

    def _route_change_timed(
        self,
        change: Change,
        *,
        snapshot: Manifest | None,
    ) -> tuple[RouteDecision, str, str | None, float]:
        try:
            path = normalize_layer_path(change.path)
        except ValueError as exc:
            return RouteDecision.REJECT, str(change.path), str(exc), 0.0

        if path == ".git" or path.startswith(".git/"):
            return (
                RouteDecision.DROP,
                path,
                ".git paths are not mutable through OCC",
                0.0,
            )

        gitignore_start = monotonic_now()
        if _is_gitignored(self._gitignore, path=path, snapshot=snapshot):
            return (
                RouteDecision.DIRECT,
                path,
                None,
                monotonic_now() - gitignore_start,
            )
        return RouteDecision.GATED, path, None, monotonic_now() - gitignore_start

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


def _requires_base_hash(change: Change) -> bool:
    return (
        isinstance(change, (WriteChange, DeleteChange))
        and change.base_hash is None
        and change.source in ("api_write", "overlay_capture")
    )


def _attach_base_hash(change: Change, base_hash: str | None) -> Change:
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
    needs_base_hash = any(_requires_base_hash(change) for change in changes)
    running_hash = base_hash_reader(path) if needs_base_hash else None
    hasher = ContentHasher()
    prepared: list[Change] = []
    for change in changes:
        next_change = (
            _attach_base_hash(change, running_hash) if _requires_base_hash(change) else change
        )
        prepared.append(next_change)
        running_hash = _next_base_hash(change, running_hash, hasher)
    return tuple(prepared)


def _next_base_hash(
    change: Change,
    running_hash: str | None,
    hasher: ContentHasher,
) -> str | None:
    if isinstance(change, WriteChange):
        return change.precomputed_hash or hasher.hash_bytes(change.final_content)
    if isinstance(change, DeleteChange):
        return None
    return running_hash


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


__all__ = [
    "BaseHashReader",
    "Router",
    "prepare_single_path_changeset",
]
