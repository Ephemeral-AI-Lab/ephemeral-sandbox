"""Team context toolkits for inherited-context reuse and sharing."""

from tools.core.base import BaseToolkit
from tools.team_context.inspect_inherited_context import inspect_inherited_context
from tools.team_context.share_briefing import share_briefing


class ContextInheritanceToolkit(BaseToolkit):
    """Read-only inspection of run-scoped inherited context."""

    def __init__(self) -> None:
        super().__init__(
            name="context_inheritance",
            description="Inspect run-scoped inherited context with live freshness and coherence.",
            tools=[inspect_inherited_context],
            instructions=(
                "Inspect same-run inherited context before reopening Atlas or restating prompt briefs.\n\n"
                "- `inspect_inherited_context` — show matching shared briefings, provenance, freshness, "
                "and the current scoped coherence token for one or more paths.\n"
                "- Treat inherited context as a reusable hint, not as live truth. If the scoped packet "
                "drifts, refresh and trust current CI/OCC state over an older brief.\n"
                "- Use this toolkit to confirm same-run reuse; use code_intelligence for live symbol, "
                "reservation, and call-chain truth."
            ),
        )


class ContextSharingToolkit(BaseToolkit):
    """Scoped promotion of high-confidence shared context."""

    def __init__(self) -> None:
        super().__init__(
            name="context_sharing",
            description="Promote high-confidence briefs into run-scoped shared context.",
            tools=[share_briefing],
            instructions=(
                "Publish high-confidence shared context for sibling reuse.\n\n"
                "- `share_briefing` — attach an inline note or artifact-backed brief to the run's shared context.\n"
                "- Treat sharing like a coordination write: only publish after the scoped packet for that "
                "slice is still fresh enough to trust. If coherence drifted, refresh first.\n"
                "- Prefer concise, scope-keyed notes that future workers can reuse without reopening the "
                "same boundary."
            ),
        )


class TeamContextToolkit(BaseToolkit):
    """Compatibility bundle for older agents that still request ``team_context``."""

    def __init__(self) -> None:
        super().__init__(
            name="team_context",
            description="Compatibility bundle for inherited-context inspection and sharing.",
            tools=[inspect_inherited_context, share_briefing],
            instructions=(
                "Legacy bundle of `context_inheritance` plus `context_sharing`.\n\n"
                "- Prefer the split toolkits for new agents so read-only reuse and coordination writes stay distinct.\n"
                "- `inspect_inherited_context` is the reuse path.\n"
                "- `share_briefing` is the publish path and should respect current scoped coherence."
            ),
        )


__all__ = [
    "ContextInheritanceToolkit",
    "ContextSharingToolkit",
    "TeamContextToolkit",
    "inspect_inherited_context",
    "share_briefing",
]
