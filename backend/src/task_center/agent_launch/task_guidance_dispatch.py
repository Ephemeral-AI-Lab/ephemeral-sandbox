"""Dispatch for the registry-driven ``<Task Guidance>`` builder.

Every agent name whose launch produces a ``<Task Guidance>`` envelope routes
through the same :func:`build_task_guidance` — there's no per-role builder.
The presence of a row here means "emit row 3 for this agent name"; absence
means "no row 3".

Helpers and subagents (``advisor``, ``explorer``) bypass the composer
entirely — they live in ``tools/ask_helper/`` and
``tools/subagent/run_subagent.py``. The explorer's identity/format prose
still lives in :func:`task_guidance.builders.build_explorer_task_guidance`,
read directly by ``tools/subagent/run_subagent.py``.
"""

from __future__ import annotations

from collections.abc import Callable

from task_center.task_guidance.builders import build_task_guidance


TaskGuidanceBuilder = Callable[..., str]


_AGENTS_WITH_TASK_GUIDANCE: frozenset[str] = frozenset(
    {
        "planner",
        "executor",
        # ``generator_verifier.md`` registers as ``name: verifier`` in its
        # frontmatter; dispatch keys match the registered agent name, not the
        # source filename.
        "verifier",
        "evaluator",
    }
)


def task_guidance_builder_for(agent_name: str) -> TaskGuidanceBuilder | None:
    """Return the single registry-driven builder, or ``None`` for no-row-3 agents."""
    if agent_name in _AGENTS_WITH_TASK_GUIDANCE:
        return build_task_guidance
    return None


__all__ = ["task_guidance_builder_for"]
