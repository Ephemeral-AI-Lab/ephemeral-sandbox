"""Route OCC changes into direct or gated prepared path groups."""

from __future__ import annotations

import asyncio
from collections import OrderedDict
from collections.abc import Callable, Sequence

from sandbox.layer_stack.changes import normalize_layer_path
from sandbox.occ.changeset.intent import (
    CommitIntent,
    PreparedChangeset,
    PreparedPathGroup,
    RouteDecision,
)
from sandbox.occ.changeset.types import (
    Change,
    DeleteChange,
    DirectChange,
    WriteChange,
)
from sandbox.occ.content.gitignore_oracle import GitignoreOracle

BaseHashReader = Callable[[str], str | None]


class OccOrchestrator:
    """Prepare direct and gated path groups for a typed OCC changeset."""

    def __init__(self, gitignore: GitignoreOracle) -> None:
        self._gitignore = gitignore

    async def prepare(
        self,
        changes: Sequence[Change],
        *,
        snapshot,
        intent: CommitIntent,
        base_hash_reader: BaseHashReader | None = None,
    ) -> PreparedChangeset:
        """Route changes and infer gated base hashes concurrently by path."""
        grouped = self._group_by_route(changes)
        prepared = await asyncio.gather(
            *(
                asyncio.to_thread(
                    self._prepare_group,
                    path,
                    route,
                    tuple(path_changes),
                    message,
                    base_hash_reader,
                )
                for path, route, path_changes, message in grouped
            )
        )
        return PreparedChangeset(
            snapshot=snapshot,
            path_groups=tuple(prepared),
            atomic=intent.atomic,
        )

    def _group_by_route(
        self,
        changes: Sequence[Change],
    ) -> list[tuple[str, RouteDecision, list[Change], str | None]]:
        grouped: OrderedDict[
            tuple[RouteDecision, str], tuple[list[Change], str | None]
        ] = OrderedDict()
        for change in changes:
            route, path, message = self._route_change(change)
            key = (route, path)
            if key not in grouped:
                grouped[key] = ([], message)
            grouped[key][0].append(change)
        return [
            (path, route, path_changes, message)
            for (route, path), (path_changes, message) in grouped.items()
        ]

    def _route_change(self, change: Change) -> tuple[RouteDecision, str, str | None]:
        try:
            path = normalize_layer_path(change.path)
        except ValueError as exc:
            return RouteDecision.REJECT, str(change.path), str(exc)

        if path == ".git" or path.startswith(".git/"):
            return RouteDecision.DROP, path, ".git paths are not mutable through OCC"

        if isinstance(change, DirectChange):
            return RouteDecision.DIRECT, path, None

        if self._gitignore.is_ignored(path):
            return RouteDecision.DIRECT, path, None
        return RouteDecision.TRACKED, path, None

    def _prepare_group(
        self,
        path: str,
        route: RouteDecision,
        changes: tuple[Change, ...],
        message: str | None,
        base_hash_reader: BaseHashReader | None,
    ) -> PreparedPathGroup:
        if route is not RouteDecision.TRACKED or base_hash_reader is None:
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
        isinstance(change, WriteChange | DeleteChange)
        and change.base_hash is None
        and change.source in ("api_write", "shell_capture")
    )


def _attach_base_hash(change: Change, base_hash: str | None) -> Change:
    if isinstance(change, WriteChange):
        return change.with_base_hash(base_hash)
    if isinstance(change, DeleteChange):
        return change.with_base_hash(base_hash)
    return change


__all__ = ["OccOrchestrator"]
