"""Unit tests for the rule factories in `notification.library`."""

from __future__ import annotations

from typing import Any

import pytest

from message.messages import ConversationMessage, TextBlock
from notification.library import make_budget_warning, make_opening_reminder
from notification.rules import dispatch_rules
from notification._runtime import SystemNotificationService


class _StubBudget:
    def __init__(self, used: int, limit: int | None) -> None:
        self.used = used
        self.limit = limit

    @property
    def fraction_used(self) -> float:
        if self.limit is None or self.limit <= 0:
            return 0.0
        return self.used / self.limit


class _StubContext:
    """Minimal QueryContext stub exposing only the surface rules use."""

    def __init__(self, used: int = 0, limit: int | None = 100) -> None:
        self._budget = _StubBudget(used, limit)
        self.notification_state: dict[str, Any] = {}

    @property
    def tool_budget(self) -> _StubBudget:
        return self._budget

    def set_used(self, used: int) -> None:
        self._budget.used = used


def _user_message(text: str = "hello") -> ConversationMessage:
    return ConversationMessage(role="user", content=[TextBlock(text=text)])


def _assistant_message(text: str = "ack") -> ConversationMessage:
    return ConversationMessage(role="assistant", content=[TextBlock(text=text)])


# ---------- make_opening_reminder --------------------------------------------


@pytest.mark.asyncio
async def test_opening_reminder_fires_on_first_turn() -> None:
    rule = make_opening_reminder("agent rules go here")
    service = SystemNotificationService()
    fired: set[str] = set()

    # First turn: no assistant message yet.
    await dispatch_rules([rule], [_user_message()], _StubContext(), service, fired)

    blocks = service.pop_pending_notifications()
    assert [b.text for b in blocks] == ["agent rules go here"]
    assert fired == {"opening_reminder"}


@pytest.mark.asyncio
async def test_opening_reminder_silent_after_assistant_replied() -> None:
    rule = make_opening_reminder("agent rules go here")
    service = SystemNotificationService()
    fired: set[str] = set()

    messages = [_user_message(), _assistant_message()]
    await dispatch_rules([rule], messages, _StubContext(), service, fired)

    assert service.pop_pending_notifications() == []
    assert fired == set()


@pytest.mark.asyncio
async def test_opening_reminder_fires_only_once_in_run() -> None:
    rule = make_opening_reminder("rules")
    service = SystemNotificationService()
    fired: set[str] = set()

    # Two consecutive opening-style invocations (still no assistant message)
    # should still emit only once thanks to fire_once dedup.
    await dispatch_rules([rule], [_user_message()], _StubContext(), service, fired)
    await dispatch_rules([rule], [_user_message()], _StubContext(), service, fired)

    blocks = service.pop_pending_notifications()
    assert len(blocks) == 1


@pytest.mark.asyncio
async def test_opening_reminder_strips_whitespace() -> None:
    rule = make_opening_reminder("\n\n  trimmed body  \n")
    service = SystemNotificationService()
    fired: set[str] = set()

    await dispatch_rules([rule], [_user_message()], _StubContext(), service, fired)

    blocks = service.pop_pending_notifications()
    assert [b.text for b in blocks] == ["trimmed body"]


# ---------- make_budget_warning ----------------------------------------------


@pytest.mark.asyncio
async def test_budget_warning_fires_at_each_threshold() -> None:
    rule = make_budget_warning(thresholds=(0.5, 0.75, 0.9))
    service = SystemNotificationService()
    fired: set[str] = set()
    ctx = _StubContext(used=0, limit=10)

    # Below 50% — silent.
    ctx.set_used(4)
    await dispatch_rules([rule], [], ctx, service, fired)
    assert service.pop_pending_notifications() == []

    # Cross 50%.
    ctx.set_used(5)
    await dispatch_rules([rule], [], ctx, service, fired)
    blocks = service.pop_pending_notifications()
    assert len(blocks) == 1 and "50%" in blocks[0].text

    # Same 50% — should not re-fire.
    await dispatch_rules([rule], [], ctx, service, fired)
    assert service.pop_pending_notifications() == []

    # Cross 75%.
    ctx.set_used(8)
    await dispatch_rules([rule], [], ctx, service, fired)
    blocks = service.pop_pending_notifications()
    assert len(blocks) == 1 and "75%" in blocks[0].text

    # Cross 90%.
    ctx.set_used(9)
    await dispatch_rules([rule], [], ctx, service, fired)
    blocks = service.pop_pending_notifications()
    assert len(blocks) == 1 and "90%" in blocks[0].text


@pytest.mark.asyncio
async def test_budget_warning_no_limit_silent() -> None:
    rule = make_budget_warning()
    service = SystemNotificationService()
    fired: set[str] = set()
    ctx = _StubContext(used=999, limit=None)

    await dispatch_rules([rule], [], ctx, service, fired)
    assert service.pop_pending_notifications() == []


@pytest.mark.asyncio
async def test_budget_warning_jumps_past_threshold() -> None:
    rule = make_budget_warning(thresholds=(0.5, 0.75, 0.9))
    service = SystemNotificationService()
    fired: set[str] = set()
    ctx = _StubContext(used=0, limit=10)

    # Jump straight to 80% — fires the 75% threshold (highest crossed by
    # this trigger evaluation; the 50% threshold is skipped by design).
    ctx.set_used(8)
    await dispatch_rules([rule], [], ctx, service, fired)
    blocks = service.pop_pending_notifications()
    # Implementation fires the lowest unfired threshold ≤ frac on each
    # invocation, so a single jump fires the smallest crossed first.
    assert len(blocks) == 1
    assert "50%" in blocks[0].text

    # Next invocation at the same fraction keeps walking the ladder.
    await dispatch_rules([rule], [], ctx, service, fired)
    blocks = service.pop_pending_notifications()
    assert len(blocks) == 1 and "75%" in blocks[0].text
