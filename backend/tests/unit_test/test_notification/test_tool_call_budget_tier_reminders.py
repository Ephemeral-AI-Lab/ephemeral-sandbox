"""Unit tests for tool-call budget tier reminders."""

from __future__ import annotations

from typing import Any

import pytest

from notification import (
    SystemNotificationService,
    dispatch_rules,
    make_terminal_tool_call_count_reminders,
)
from tools._framework.core.results import ToolResult


class _StubContext:
    """Minimal QueryContext stub used by the tier reminder rules."""

    def __init__(
        self,
        *,
        terminal_result: ToolResult | None = None,
        tool_calls_used: int = 0,
        tool_call_limit: int = 20,
    ) -> None:
        self.terminal_result = terminal_result
        self.tool_calls_used = tool_calls_used
        self.tool_call_limit = tool_call_limit
        self.notification_fired: set[str] = set()
        self.notification_state: dict[str, Any] = {}


@pytest.mark.asyncio
async def test_emits_each_budget_tier_once_as_usage_crosses_thresholds() -> None:
    rules = make_terminal_tool_call_count_reminders()
    service = SystemNotificationService()
    ctx = _StubContext(tool_calls_used=14, tool_call_limit=20)

    await dispatch_rules(rules, [], ctx, service)
    assert service.pop_pending_notifications() == []

    ctx.tool_calls_used = 15
    await dispatch_rules(rules, [], ctx, service)
    await dispatch_rules(rules, [], ctx, service)
    blocks = service.pop_pending_notifications()
    assert [block.text for block in blocks] == [
        "Tool-call budget warning: 75% of the planned budget has been used "
        "(15/20 tool calls). Submit a terminal tool as soon as the work is "
        "complete; the run will fail at 30 tool calls (15 remaining)."
    ]

    ctx.tool_calls_used = 20
    await dispatch_rules(rules, [], ctx, service)
    blocks = service.pop_pending_notifications()
    assert len(blocks) == 1
    assert "100% of the planned budget" in blocks[0].text
    assert "(20/20 tool calls)" in blocks[0].text

    ctx.tool_calls_used = 25
    await dispatch_rules(rules, [], ctx, service)
    blocks = service.pop_pending_notifications()
    assert len(blocks) == 1
    assert "125% of the planned budget" in blocks[0].text
    assert "(25/20 tool calls)" in blocks[0].text

    assert ctx.notification_fired == {
        "tool_call_budget_75_percent",
        "tool_call_budget_100_percent",
        "tool_call_budget_125_percent",
    }


@pytest.mark.asyncio
async def test_emits_all_newly_crossed_tiers_in_rule_order() -> None:
    rules = make_terminal_tool_call_count_reminders()
    service = SystemNotificationService()
    ctx = _StubContext(tool_calls_used=25, tool_call_limit=20)

    await dispatch_rules(rules, [], ctx, service)

    blocks = service.pop_pending_notifications()
    labels = [
        block.text.split(":", 1)[1].strip().split(" ", 1)[0]
        for block in blocks
    ]
    assert labels == [
        "75%",
        "100%",
        "125%",
    ]


@pytest.mark.asyncio
async def test_silent_once_terminal_result_set() -> None:
    rules = make_terminal_tool_call_count_reminders()
    service = SystemNotificationService()
    ctx = _StubContext(
        terminal_result=ToolResult(output="done", is_error=False, is_terminal=True),
        tool_calls_used=25,
        tool_call_limit=20,
    )

    await dispatch_rules(rules, [], ctx, service)

    assert service.pop_pending_notifications() == []
    assert ctx.notification_fired == set()
