"""System notification service used by tool execution and hooks."""

from __future__ import annotations

from collections.abc import Awaitable, Callable
from dataclasses import dataclass, field

from message.messages import SystemReminderBlock
from notification.events import SystemNotification


@dataclass
class SystemNotificationService:
    """Per-tool-call notification sink for hooks and tools.

    Notifications are emitted to subscribers immediately and retained as
    ``SystemReminderBlock`` objects so the query loop can make them visible to
    the agent on the next turn.
    """

    emit: Callable[[SystemNotification], Awaitable[None]] | None = None
    _reminders: list[SystemReminderBlock] = field(default_factory=list)

    async def notify_system(self, text: str, *, category: str = "") -> None:
        if not text:
            return
        event = SystemNotification(text=text, category=category)
        self._reminders.append(SystemReminderBlock(text=text, category=category))
        if self.emit is not None:
            await self.emit(event)

    async def notify(self, text: str, *, category: str = "") -> None:
        await self.notify_system(text, category=category)

    def drain_reminders(self) -> list[SystemReminderBlock]:
        reminders = list(self._reminders)
        self._reminders.clear()
        return reminders
