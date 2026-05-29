"""Built-in notification rule factories.

Per-agent definitions assemble these factories into notification rule lists.
"""

from __future__ import annotations

from notification.rules.must_submit_terminal_tool import make_terminal_call_reminder
from notification.rules.terminal_tool_call_count_reminder import (
    make_tool_call_budget_tier_reminders,
    make_terminal_tool_call_count_reminders,
)


__all__ = [
    "make_terminal_call_reminder",
    "make_terminal_tool_call_count_reminders",
    "make_tool_call_budget_tier_reminders",
]
