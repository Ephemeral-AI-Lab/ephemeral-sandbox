"""Commit options and prepared OCC path groups."""

from __future__ import annotations

from dataclasses import dataclass, field
from enum import Enum

from sandbox.layer_stack.manifest import Manifest
from sandbox.occ.changeset.types import Change


class RouteDecision(str, Enum):
    GATED = "gated"
    DIRECT = "direct"
    DROP = "drop"
    REJECT = "reject"

    # Backward-compatible aliases for older callers. New code should use
    # DIRECT/GATED so route names describe what will happen, not what is
    # being skipped.
    OCC_GATED_MERGE = "gated"
    OCC_SKIPPED_MERGE = "direct"


@dataclass(frozen=True)
class PreparedPathGroup:
    """Ordered changes for one normalized path and route decision."""

    path: str
    route: RouteDecision
    changes: tuple[Change, ...]
    message: str | None = None


@dataclass(frozen=True)
class CommitOptions:
    """Request-level OCC commit options.

    ``atomic`` defaults to ``True``: a multi-path changeset is published only
    if every path validates. If any path fails (ABORTED_OVERLAP,
    ABORTED_VERSION, FAILED, or REJECTED), no path lands. Callers that want
    best-effort partial publish must opt out explicitly with
    ``atomic=False``.
    """

    atomic: bool = True


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
