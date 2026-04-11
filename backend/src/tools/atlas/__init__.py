"""Atlas toolkit — planner-side read access to the Project Atlas (Phase 2).

Exposes a single tool, :func:`atlas_lookup`, for planners to consult the
persistent scout brief cache. Planners never emit atlas work items
directly; fresh exploration happens through `scout`, and reusable scout
results may be persisted by the runtime.
"""

from tools.atlas.lookup import atlas_lookup
from tools.core.base import BaseToolkit


class AtlasToolkit(BaseToolkit):
    """Read-only access to the persistent Project Atlas for planner agents."""

    def __init__(self) -> None:
        super().__init__(
            name="atlas",
            description="Persistent cross-run scout brief cache (Project Atlas).",
            tools=[atlas_lookup],
            instructions=(
                "Use Atlas only for cross-run structural reuse of canonical subsystem scopes. "
                "Do not use Atlas for same-run edit awareness, conflict detection, live symbol placement, "
                "or semantic understanding; those belong to shared briefings, fresh scout results, "
                "and the `code_intelligence` toolkit.\n"
                "Look up cached scout briefs for one or more subsystems. "
                "`atlas_lookup` returns a decision per subsystem:\n"
                "- `use`: a staged artifact ref is included — attach it to a worker briefing.\n"
                "- `refresh`: the cached chunk is stale. Treat atlas as unavailable "
                "for this planning turn and fall back to fresh exploration.\n"
                "- `scout`: no cached brief exists. Fall back to fresh exploration.\n"
                "In a changing repo, exhaust fresh same-run context first: shared briefings, "
                "current-turn scout outputs, and live `ci_scoped_status(...)` anchoring beat Atlas.\n"
                "Use Atlas when you can already name a real owner scope and want a cheap answer to "
                "'do we already have a reusable structural brief for this slice?'\n"
                "On resumed or repeated work, once same-run reuse is exhausted and the owner scope is "
                "already exact, do one `atlas_lookup(...)` before spawning a duplicate scout.\n"
                "Semantic questions ('how does X work', 'why does Y exist') must "
                "always go to a fresh scout, never the atlas."
            ),
        )


__all__ = ["AtlasToolkit", "atlas_lookup"]
