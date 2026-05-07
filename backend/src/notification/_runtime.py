"""System notification service used by agent runs, tool execution, and hooks."""

from __future__ import annotations

from collections.abc import Awaitable, Callable
from dataclasses import dataclass, field
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from message.messages import SystemNotificationBlock


@dataclass(frozen=True)
class SystemNotification:
    """Engine-generated notification visible to the user and the agent."""

    text: str
    agent_name: str = ""
    run_id: str = ""


@dataclass
class SystemNotificationService:
    """Run-scoped notification sink for hooks, tools, and runtime code.

    Agent runs emit notifications as stream events only; standalone tool
    executions can still pass ``emit`` or drain notifications into tool
    metadata for backwards compatibility.
    """

    emit: Callable[[SystemNotification], Awaitable[None]] | None = None
    _registered_agent_run: bool = field(default=False, init=False, repr=False)
    _notifications: list[SystemNotificationBlock] = field(default_factory=list, repr=False)
    _events: list[SystemNotification] = field(default_factory=list, init=False, repr=False)

    @property
    def has_registered_agent_run(self) -> bool:
        """Return True when this service is owned by an agent run."""
        return self._registered_agent_run

    def register_agent_run(self) -> None:
        """Mark the service as owned by a live agent run."""
        self._registered_agent_run = True

    async def notify_system(self, text: str) -> None:
        if not text:
            return
        from message.messages import SystemNotificationBlock

        event = SystemNotification(text=text)
        self._notifications.append(SystemNotificationBlock(text=text))
        if self.emit is not None:
            await self.emit(event)
        else:
            self._events.append(event)

    def flush_events(self) -> list[SystemNotification]:
        """Return pending notifications without appending transcript messages."""
        events = list(self._events)
        if not events and self._notifications:
            events = [
                SystemNotification(text=notification.text)
                for notification in self._notifications
            ]
        self._notifications.clear()
        self._events.clear()
        return events

    def pop_pending_notifications(self) -> list[SystemNotificationBlock]:
        """Drain transcript-bound notification blocks.

        In agent runs (registered via ``register_agent_run``) leaves
        ``_events`` untouched so the stream-side flush
        (``flush_events`` / ``flush_system_notifications``) still emits
        these events to the user UI. In standalone tool execution where
        nothing else drains ``_events``, clears them too to keep memory
        bounded.
        """
        notifications = list(self._notifications)
        self._notifications.clear()
        if not self._registered_agent_run:
            self._events.clear()
        return notifications
