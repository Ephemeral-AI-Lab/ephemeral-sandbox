"""Public notification API."""

from notification.runtime import (
    SystemNotification,
    SystemNotificationService,
    ensure_system_notification_service,
    flush_system_notification_events,
)
from notification.metadata import (
    SYSTEM_NOTIFICATIONS_METADATA_KEY,
    serialize_system_notifications,
)
from notification.rules import NotificationRule, dispatch_rules
from notification.rules import (
    make_terminal_call_reminder,
    make_terminal_tool_call_count_reminders,
    make_tool_call_budget_tier_reminders,
)

__all__ = [
    "NotificationRule",
    "SYSTEM_NOTIFICATIONS_METADATA_KEY",
    "SystemNotification",
    "SystemNotificationService",
    "dispatch_rules",
    "ensure_system_notification_service",
    "flush_system_notification_events",
    "make_terminal_call_reminder",
    "make_terminal_tool_call_count_reminders",
    "make_tool_call_budget_tier_reminders",
    "serialize_system_notifications",
]
