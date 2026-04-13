"""Builders for system notifications emitted by the engine query loop.

Each helper returns ``None`` when no notification should fire, or a
``(history_message, stream_event)`` pair the loop appends to
``display_messages`` and yields to subscribers respectively. Keeping
this logic out of :mod:`engine.core.query` makes the loop body about
control flow only and gives notifications a single, easy-to-test home.
"""

from __future__ import annotations

import math
from typing import TYPE_CHECKING

from message.messages import ConversationMessage
from message.stream_events import SystemNotification

if TYPE_CHECKING:
    from engine.core.query import QueryContext


def _budget_warning_steps(context: "QueryContext") -> str:
    role = ""
    if context.tool_metadata is not None:
        role = str(context.tool_metadata.get("role", "") or "")

    if role == "planner":
        return (
            "1. Stop exploring and shaping new lanes immediately.\n"
            "2. Call context_changed_since() if you have not already.\n"
            "3. Call submit_plan() with the strongest decomposition you can defend right now."
        )
    if role == "replanner":
        return (
            "1. Stop reopening ownership questions immediately.\n"
            "2. Call context_changed_since() if you have not already.\n"
            "3. Call submit_replan() with the corrective split you can already justify."
        )
    if role == "reviewer":
        return (
            "1. Run one final exact verification command (daytona_codeact) only if you still need decisive evidence.\n"
            "2. Call context_changed_since() if you have not already.\n"
            "3. If the result is green, call submit_summary(); if it is red, call request_replan() "
            "(or request_retry() only for transient runtime faults)."
        )
    return (
        "1. Run one final verification command (daytona_codeact) on your most critical test.\n"
        "2. Call context_changed_since() if you have not already.\n"
        "3. If you are green, call submit_summary(); if you are blocked or red, call request_replan() "
        "(or request_retry() only for transient runtime faults)."
    )


def build_budget_warning(
    context: "QueryContext",
) -> tuple[ConversationMessage, SystemNotification] | None:
    """Warn the agent that its ``tool_call_limit`` is nearly exhausted.

    Fires when:
      - ``tool_call_limit`` is set, AND
      - used budget has reached 75% of the limit OR only 1 call remains, AND
      - the budget is not yet fully exhausted (``remaining > 0``).

    Returns ``(message, event)``: the loop appends ``message`` to
    ``display_messages`` so the agent's next turn sees it, then yields
    ``event`` so subscribers (eval harness, UI) get a structured notice.
    """
    limit = context.tool_call_limit
    if limit is None:
        return None
    remaining = limit - context.tool_calls_used
    if remaining <= 0:
        return None  # exhausted — handled by loop termination
    used_threshold = max(1, math.ceil(limit * 0.75))
    should_warn = context.tool_calls_used in {used_threshold, limit - 1}
    if not should_warn:
        return None
    if context.last_budget_warning_remaining == remaining:
        return None
    context.last_budget_warning_remaining = remaining
    text = (
        f"[budget warning] Only {remaining} of {limit} tool calls remain "
        f"({context.tool_calls_used} already used). "
        f"Stop editing and exploring immediately. Your next actions must be:\n"
        f"{_budget_warning_steps(context)}\n"
        f"Do NOT start new edits, file reads, or debugging loops. Submit now."
    )
    return (
        ConversationMessage.from_user_text(text),
        SystemNotification(text=text, category="budget_warning"),
    )
