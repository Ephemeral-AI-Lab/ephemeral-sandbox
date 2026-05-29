"""Reminder rule for agents that must submit a terminal tool."""

from __future__ import annotations

import math
from typing import TYPE_CHECKING

from notification.rules.model import MessageList, NotificationRule

if TYPE_CHECKING:
    from engine.api import QueryContext


def make_terminal_call_reminder() -> NotificationRule:
    """Nudge the agent to submit a terminal tool."""

    def _trigger(messages: MessageList, context: "QueryContext") -> bool:
        return (
            context.terminal_result is None
            and any(m.role == "assistant" for m in messages)
        )

    def _body(messages: MessageList, context: "QueryContext") -> str:
        del messages
        names = ", ".join(sorted(context.terminal_tools))
        used = context.tool_calls_used
        limit = context.tool_call_limit
        ceiling = math.ceil(1.5 * limit)
        turns_remaining = max(0, ceiling - used)
        return (
            f"You have not submitted a terminal tool. Deliver your result "
            f"by calling one of: {names}. Budget: {used}/{limit} tool calls "
            f"used; the run will fail at {ceiling} tool calls "
            f"({turns_remaining} remaining)."
        )

    return NotificationRule(
        name="terminal_call_reminder",
        body=_body,
        trigger=_trigger,
        fire_once=False,
    )
