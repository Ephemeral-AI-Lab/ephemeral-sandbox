"""Soft reminder for disabled generator complex-task request starts after edit."""

from __future__ import annotations

from typing import Any

from notification.rules import NotificationRule
from tools.submission.hooks.request_complex_task_before_edit_gate import (
    generator_has_edited,
)


def make_request_after_edit_reminder() -> NotificationRule:
    def _trigger(messages: list[Any], context: Any) -> bool:
        del context
        return generator_has_edited(messages)

    def _body(messages: list[Any], context: Any) -> str:
        del messages, context
        return (
            "request_complex_task_solution is disabled after the first edit. "
            "Finish through this generator agent's success or failure terminal."
        )

    return NotificationRule(
        name="request_complex_task_after_edit",
        trigger=_trigger,
        body=_body,
    )
