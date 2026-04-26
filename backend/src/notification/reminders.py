"""System reminder metadata conversion helpers."""

from __future__ import annotations

from message.messages import SystemReminderBlock


SYSTEM_REMINDERS_METADATA_KEY = "system_reminders"


def serialize_system_reminders(reminders: list[SystemReminderBlock]) -> list[dict[str, str]]:
    return [block.model_dump(mode="json") for block in reminders]


def system_reminders_from_metadata(metadata: dict[str, object]) -> list[SystemReminderBlock]:
    raw = metadata.get(SYSTEM_REMINDERS_METADATA_KEY)
    if not isinstance(raw, list):
        return []
    reminders: list[SystemReminderBlock] = []
    for item in raw:
        if isinstance(item, SystemReminderBlock):
            reminders.append(item)
            continue
        if isinstance(item, dict):
            try:
                reminders.append(SystemReminderBlock.model_validate(item))
            except Exception:
                continue
    return reminders
