"""System notification metadata conversion helpers."""

from __future__ import annotations

from message.messages import SystemNotificationBlock


SYSTEM_NOTIFICATIONS_METADATA_KEY = "system_notifications"


def serialize_system_notifications(
    notifications: list[SystemNotificationBlock],
) -> list[dict[str, str]]:
    return [block.model_dump(mode="json") for block in notifications]
