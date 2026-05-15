"""TaskCenter submission notification trigger factories."""

from __future__ import annotations

from notification import NotificationRule
from tools.submission.notification_triggers.request_goal_after_edit import (
    make_goal_request_after_edit_reminder,
)
from tools.submission.notification_triggers.resolver_limit import (
    make_resolver_limit_reminder,
)


def resolve_harness_notification_triggers(
    trigger_ids: list[str],
) -> list[NotificationRule]:
    factories = {
        "request_goal_after_edit": make_goal_request_after_edit_reminder,
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
    "make_goal_request_after_edit_reminder",
    "make_resolver_limit_reminder",
    "resolve_harness_notification_triggers",
]
