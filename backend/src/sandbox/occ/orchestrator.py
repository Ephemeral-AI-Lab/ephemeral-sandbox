"""Route OCC changes into OCC-skipped or OCC-gated prepared path groups."""

from __future__ import annotations

from collections import OrderedDict
from collections.abc import Callable, Sequence
from typing import Optional

from sandbox.layer_stack.changes import normalize_layer_path
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
from sandbox.occ.content.gitignore_oracle import GitignoreOracle
from sandbox.runtime.async_bridge import run_sync_in_executor

BaseHashReader = Callable[[str], Optional[str]]


class OccOrchestrator:
    """Prepare OCC-skipped and OCC-gated path groups for a typed changeset."""

    def __init__(self, gitignore: GitignoreOracle) -> None:
        self._gitignore = gitignore

    async def prepare(
        self,
        changes: Sequence[Change],
        *,
        snapshot,
        options: CommitOptions,
        base_hash_reader: BaseHashReader | None = None,
    ) -> PreparedChangeset:
        """Route changes and infer gated base hashes concurrently by path."""
        return await run_sync_in_executor(
            self.prepare_sync,
            changes,
            snapshot=snapshot,
            options=options,
            base_hash_reader=base_hash_reader,
        )

    def prepare_sync(
        self,
        changes: Sequence[Change],
        *,
        snapshot,
        options: CommitOptions,
        base_hash_reader: BaseHashReader | None = None,
    ) -> PreparedChangeset:
        """Route changes and infer gated base hashes synchronously by path."""
        grouped = self._group_by_route(changes, snapshot=snapshot)
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
        return PreparedChangeset(
            snapshot=snapshot,
            path_groups=prepared,
            atomic=options.atomic,
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
            return RouteDecision.OCC_SKIPPED_MERGE, path, None
        return RouteDecision.OCC_GATED_MERGE, path, None

    def _prepare_group(
        self,
        path: str,
        route: RouteDecision,
        changes: tuple[Change, ...],
        message: str | None,
        base_hash_reader: BaseHashReader | None,
    ) -> PreparedPathGroup:
        if route is not RouteDecision.OCC_GATED_MERGE or base_hash_reader is None:
            return PreparedPathGroup(
                path=path,
                route=route,
                changes=changes,
                message=message,
            )

        needs_base_hash = any(_requires_base_hash(change) for change in changes)
        base_hash = base_hash_reader(path) if needs_base_hash else None
        prepared_changes = tuple(
            _attach_base_hash(change, base_hash)
            if _requires_base_hash(change)
            else change
            for change in changes
        )
        return PreparedPathGroup(
            path=path,
            route=route,
            changes=prepared_changes,
            base_hash=base_hash,
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


def _is_gitignored(
    oracle: GitignoreOracle,
    *,
    path: str,
    snapshot: Manifest | None,
) -> bool:
    if snapshot is not None:
        snapshot_oracle = getattr(oracle, "is_ignored_in_snapshot", None)
        if callable(snapshot_oracle):
            return bool(snapshot_oracle(path, snapshot))
    return oracle.is_ignored(path)


__all__ = ["OccOrchestrator"]
