"""TaskCenter submission notification trigger factories."""

from __future__ import annotations

from notification import NotificationRule
from tools.submission.notification_triggers.request_workflow_after_edit import (
    make_workflow_request_after_edit_reminder,
)


def resolve_harness_notification_triggers(
    trigger_ids: list[str],
) -> list[NotificationRule]:
    factories = {
        "request_workflow_after_edit": make_workflow_request_after_edit_reminder,
    }
    rules: list[NotificationRule] = []
    for trigger_id in trigger_ids:
        factory = factories.get(trigger_id)
        if factory is None:
            raise ValueError(f"Unknown harness notification trigger {trigger_id!r}.")
        rules.append(factory())
    return rules


__all__ = [
    "make_workflow_request_after_edit_reminder",
    "resolve_harness_notification_triggers",
]
