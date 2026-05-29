"""Soft reminder tests for Phase 03 submission rules."""

from __future__ import annotations

from types import SimpleNamespace

import pytest

from message.message import Message, ToolUseBlock
from notification import dispatch_rules
from notification import SystemNotificationService
from tools.submission.notification_triggers import (
    make_workflow_request_after_edit_reminder,
    resolve_harness_notification_triggers,
)

pytestmark = pytest.mark.asyncio


def _edit_messages() -> list[Message]:
    return [
        Message(
            role="assistant",
            content=[ToolUseBlock(tool_use_id="toolu_edit", name="shell", input={})],
        )
    ]


async def _dispatch(rule, messages, context):
    service = SystemNotificationService()
    context.notification_fired = set()
    await dispatch_rules([rule], messages, context, service)
    return service.pop_pending_notifications()


async def test_after_edit_reminder_fires_once() -> None:
    ctx = SimpleNamespace(tool_metadata=None, cwd="/tmp")

    notifications = await _dispatch(
        make_workflow_request_after_edit_reminder(),
        _edit_messages(),
        ctx,
    )

    assert len(notifications) == 1
    assert "submit_execution_handoff is meant for delegating before edits begin" in notifications[0].text


async def test_resolve_harness_notification_triggers_rejects_unknown() -> None:
    with pytest.raises(ValueError):
        resolve_harness_notification_triggers(["missing"])
