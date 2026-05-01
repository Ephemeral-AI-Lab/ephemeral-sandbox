"""ContextScope — discriminated-union surface every recipe sees.

The scope carries identity (request / segment / graph / task ids) and helper
parent references. It does **not** carry store handles; those live on
:class:`ContextEngineDeps` so recipes can be swapped without touching call
sites.
"""

from __future__ import annotations

from dataclasses import dataclass

from task_center.context_engine.errors import RecipeScopeError


@dataclass(frozen=True, slots=True)
class ContextScope:
    """Identity surface threaded through resolver + engine + recipes."""

    request_id: str

    # Optional identity fields — recipes declare which of these they need.
    segment_id: str | None = None
    harness_graph_id: str | None = None
    task_id: str | None = None

    # Helper-spawn fields — present only when a helper (advisor / resolver) is
    # being launched by a parent agent via ``ask_advisor`` / ``run_subagent``.
    parent_packet_id: str | None = None
    parent_task_id: str | None = None

    def assert_fields(self, required: frozenset[str]) -> None:
        """Raise :class:`RecipeScopeError` if any required field is None."""
        missing = sorted(f for f in required if getattr(self, f, None) is None)
        if missing:
            raise RecipeScopeError(
                f"ContextScope is missing required fields: {missing!r}"
            )
