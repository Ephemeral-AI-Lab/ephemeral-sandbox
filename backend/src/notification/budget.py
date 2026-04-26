"""Budget-related system notification builders."""

from __future__ import annotations

import math
from typing import TYPE_CHECKING

from message.messages import ConversationMessage
from notification.events import SystemNotification

if TYPE_CHECKING:
    from engine.core.query import QueryContext


def _budget_warning_steps(context: "QueryContext") -> str:
    terminal_tools = ", ".join(sorted(context.terminal_tools)) or "the configured terminal tool"

    remaining = context.tool_call_limit - context.tool_calls_used if context.tool_call_limit else 0
    if remaining <= 1:
        return (
            f"1. Use this last call for {terminal_tools}.\n"
            "2. Do not spend the final call on more investigation, mutation, or cleanup."
        )
    return (
        f"1. Reserve one call for {terminal_tools}; never spend the last tool call on investigation, mutation, or cleanup.\n"
        "2. Continue only with a bounded known fix, required diagnostic, or exact verification that still leaves a terminal call.\n"
        "3. When only the terminal call remains, call the role-correct terminal tool with the best available evidence."
    )


def build_budget_warning(
    context: "QueryContext",
) -> tuple[ConversationMessage, SystemNotification] | None:
    """Warn the agent that its ``tool_call_limit`` is nearly exhausted."""

    limit = context.tool_call_limit
    if limit is None:
        return None
    remaining = limit - context.tool_calls_used
    if remaining <= 0:
        return None
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
        f"This is an advisory warning, not a terminal trigger. Terminal submission counts against this budget; "
        f"keep one call reserved for the role-correct terminal tool. "
        f"Prepare the terminal path while using any remaining safe calls deliberately:\n"
        f"{_budget_warning_steps(context)}\n"
        f"Do not spend the final reserved call on non-terminal mutation or investigation."
    )
    return (
        ConversationMessage.from_user_text(text),
        SystemNotification(text=text, category="budget_warning"),
    )
