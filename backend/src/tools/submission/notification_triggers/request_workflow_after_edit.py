"""Soft reminder nudging delegation to happen before the first edit."""

from __future__ import annotations

from typing import Any

from message.message import Message, ToolUseBlock
from notification import NotificationRule


_EDIT_TOOL_NAMES = frozenset({"write_file", "edit_file", "shell"})


def _generator_has_edited(messages: list[Any]) -> bool:
    for message in messages:
        if not isinstance(message, Message):
            continue
        for block in message.content:
            if isinstance(block, ToolUseBlock) and block.name in _EDIT_TOOL_NAMES:
                return True
    return False


def make_workflow_request_after_edit_reminder() -> NotificationRule:
    def _trigger(messages: list[Any], context: Any) -> bool:
        del context
        return _generator_has_edited(messages)

    def _body(messages: list[Any], context: Any) -> str:
        del messages, context
        return (
            "submit_execution_handoff is meant for delegating before edits begin. "
            "Once this generator has edited, prefer finishing through its own "
            "success or blocker terminal."
        )

    return NotificationRule(
        name="request_workflow_after_edit",
        trigger=_trigger,
        body=_body,
    )
