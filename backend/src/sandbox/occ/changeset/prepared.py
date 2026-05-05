"""Commit options and prepared OCC path groups."""

from __future__ import annotations

from dataclasses import dataclass, field
from enum import Enum

from sandbox.layer_stack.manifest import Manifest
from sandbox.occ.changeset.types import Change


class RouteDecision(str, Enum):
    OCC_GATED_MERGE = "occ_gated_merge"
    OCC_SKIPPED_MERGE = "occ_skipped_merge"
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
class CommitOptions:
    """Request-level OCC commit options and metadata."""

    atomic: bool = False
    caller_id: str = ""
    description: str = ""


@dataclass(frozen=True)
class PreparedChangeset:
    """Routed changeset consumed by the commit transaction."""

    snapshot: Manifest | None
    path_groups: tuple[PreparedPathGroup, ...]
    atomic: bool
    timings: dict[str, float] = field(default_factory=dict)


__all__ = [
    "CommitOptions",
    "PreparedChangeset",
    "PreparedPathGroup",
    "RouteDecision",
]
