"""Route OCC changes into direct or gated prepared path groups."""

from __future__ import annotations

from collections import OrderedDict
from collections.abc import Callable, Sequence

from sandbox.layer_stack.changes import normalize_layer_path
from sandbox.layer_stack.manifest import Manifest
from sandbox.occ.changeset import (
    CommitOptions,
    PreparedChangeset,
    PreparedPathGroup,
    RouteDecision,
)
from sandbox.occ.changeset import (
    Change,
    DeleteChange,
    WriteChange,
)
from sandbox.occ.gitignore import (
    GitignoreMatcher,
    SnapshotGitignoreMatcher,
)
from sandbox.occ.hashing import ContentHasher
from sandbox.timing_keys import TimingKey
from sandbox._shared.clock import monotonic_now

BaseHashReader = Callable[[str], str | None]


class ChangesetPreparer:
    """Prepare direct and gated path groups for a typed changeset."""

    def __init__(self, gitignore: GitignoreMatcher) -> None:
        self._gitignore = gitignore
        self._snapshot_gitignore: SnapshotGitignoreMatcher | None = (
            gitignore if isinstance(gitignore, SnapshotGitignoreMatcher) else None
        )

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
            route, path, message, _ = self._route_change_timed(change, snapshot=snapshot)
            key = (route, path)
            if key not in grouped:
                grouped[key] = ([], message)
            grouped[key][0].append(change)
        return [
            (path, route, path_changes, message)
            for (route, path), (path_changes, message) in grouped.items()
        ]

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
        if self._is_gitignored(path, snapshot):
            return (
                RouteDecision.DIRECT,
                path,
                None,
                monotonic_now() - gitignore_start,
            )
        return RouteDecision.GATED, path, None, monotonic_now() - gitignore_start

    def _is_gitignored(self, path: str, snapshot: Manifest | None) -> bool:
        if snapshot is None:
            return self._gitignore.is_ignored(path)
        matcher = self._snapshot_gitignore
        if matcher is None:
            raise TypeError(
                "snapshot-aware OCC routing requires "
                "SnapshotGitignoreMatcher.is_ignored_in_snapshot"
            )
        return matcher.is_ignored_in_snapshot(path, snapshot)

    def _prepare_group(
        self,
        path: str,
        route: RouteDecision,
        changes: tuple[Change, ...],
        message: str | None,
        base_hash_reader: BaseHashReader | None,
    ) -> PreparedPathGroup:
        if (
            route is not RouteDecision.GATED
            or base_hash_reader is None
            or not any(_requires_base_hash(change) for change in changes)
        ):
            return PreparedPathGroup(path=path, route=route, changes=changes, message=message)

        running_hash = base_hash_reader(path)
        hasher = ContentHasher()
        prepared: list[Change] = []
        for change in changes:
            if _requires_base_hash(change):
                prepared.append(_attach_base_hash(change, running_hash))
            else:
                prepared.append(change)
            if isinstance(change, WriteChange):
                running_hash = change.precomputed_hash or hasher.hash_bytes(change.final_content)
            elif isinstance(change, DeleteChange):
                running_hash = None
        return PreparedPathGroup(path=path, route=route, changes=tuple(prepared), message=message)


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


def prepare_single_path_changeset(
    change: Change,
    *,
    snapshot: Manifest,
    gitignore: SnapshotGitignoreMatcher,
    base_hash_reader: BaseHashReader | None = None,
    atomic: bool = False,
) -> PreparedChangeset:
    """Prepare one path through the shared router fast branch."""
    return ChangesetPreparer(gitignore).prepare_single_path_sync(
        change,
        snapshot=snapshot,
        base_hash_reader=base_hash_reader,
        atomic=atomic,
    )


__all__ = [
    "BaseHashReader",
    "ChangesetPreparer",
    "prepare_single_path_changeset",
]
