"""TaskCenter submission notification trigger factories."""

from __future__ import annotations

from notification.rules import NotificationRule
from tools.submission.notification_triggers.request_complex_task_after_edit import (
    make_request_after_edit_reminder,
)
from tools.submission.notification_triggers.resolver_limit import (
    make_resolver_limit_reminder,
)


def resolve_harness_notification_triggers(
    trigger_ids: list[str],
) -> list[NotificationRule]:
    factories = {
        "request_complex_task_after_edit": make_request_after_edit_reminder,
        "resolver_limit": make_resolver_limit_reminder,
    }
    rules: list[NotificationRule] = []
    for trigger_id in trigger_ids:
        factory = factories.get(trigger_id)
        if factory is None:
            raise ValueError(f"Unknown harness notification trigger {trigger_id!r}.")
        rules.append(factory())
    return rules


__all__ = [
    "make_request_after_edit_reminder",
    "make_resolver_limit_reminder",
    "resolve_harness_notification_triggers",
]
