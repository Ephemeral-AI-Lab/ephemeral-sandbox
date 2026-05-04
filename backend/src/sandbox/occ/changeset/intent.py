"""Commit intent and prepared OCC path groups."""

from __future__ import annotations

from dataclasses import dataclass
from enum import StrEnum

from sandbox.layer_stack.manifest import Manifest
from sandbox.occ.changeset.types import Change


class RouteDecision(StrEnum):
    TRACKED = "tracked"
    DIRECT = "direct"
    DROP = "drop"
    REJECT = "reject"


@dataclass(frozen=True)
class PreparedPathGroup:
    """Ordered changes for one normalized path and route decision."""

    path: str
    route: RouteDecision
    changes: tuple[Change, ...]
    base_hash: str | None = None
    message: str | None = None


@dataclass(frozen=True)
class CommitIntent:
    """Request-level OCC commit intent."""

    atomic: bool = False
    caller_id: str = ""
    description: str = ""


@dataclass(frozen=True)
class PreparedChangeset:
    """Routed changeset consumed by the commit transaction."""

    snapshot: Manifest | None
    path_groups: tuple[PreparedPathGroup, ...]
    atomic: bool


__all__ = [
    "CommitIntent",
    "PreparedChangeset",
    "PreparedPathGroup",
    "RouteDecision",
]
