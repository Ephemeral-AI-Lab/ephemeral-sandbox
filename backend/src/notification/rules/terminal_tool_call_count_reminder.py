"""Terminal tool-call count reminder rules."""

from __future__ import annotations

import math
from typing import TYPE_CHECKING

from notification.rules.model import MessageList, NotificationRule

if TYPE_CHECKING:
    from engine.api import QueryContext


_TOOL_CALL_BUDGET_TIERS: tuple[tuple[str, int, int], ...] = (
    ("75%", 3, 4),
    ("100%", 1, 1),
    ("125%", 5, 4),
)


def _ceil_ratio(value: int, numerator: int, denominator: int) -> int:
    return (value * numerator + denominator - 1) // denominator


def _make_terminal_tool_call_count_reminder(
    label: str,
    numerator: int,
    denominator: int,
) -> NotificationRule:
    rule_name = f"tool_call_budget_{label.removesuffix('%')}_percent"

    def _trigger(messages: MessageList, context: "QueryContext") -> bool:
        del messages
        threshold = _ceil_ratio(context.tool_call_limit, numerator, denominator)
        return context.terminal_result is None and context.tool_calls_used >= threshold

    def _body(messages: MessageList, context: "QueryContext") -> str:
        del messages
        used = context.tool_calls_used
        limit = context.tool_call_limit
        ceiling = math.ceil(1.5 * limit)
        turns_remaining = max(0, ceiling - used)
        return (
            f"Tool-call budget warning: {label} of the planned budget has been "
            f"used ({used}/{limit} tool calls). Submit a terminal tool as soon "
            f"as the work is complete; the run will fail at {ceiling} tool "
            f"calls ({turns_remaining} remaining)."
        )

    return NotificationRule(
        name=rule_name,
        body=_body,
        trigger=_trigger,
    )


def make_terminal_tool_call_count_reminders() -> list[NotificationRule]:
    """Warn once when tool usage reaches 75%, 100%, and 125% of the limit."""

    return [
        _make_terminal_tool_call_count_reminder(label, numerator, denominator)
        for label, numerator, denominator in _TOOL_CALL_BUDGET_TIERS
    ]


make_tool_call_budget_tier_reminders = make_terminal_tool_call_count_reminders
