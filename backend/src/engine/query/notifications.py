"""Notification plumbing shared by the query loop."""

from __future__ import annotations

from message.stream_events import StreamEvent
from notification._runtime import SystemNotificationService
from providers.types import UsageSnapshot
from tools import ExecutionMetadata


def ensure_system_notification_service(
    metadata: ExecutionMetadata | None,
) -> SystemNotificationService:
    service = metadata.system_notification_service if metadata is not None else None
    if not isinstance(service, SystemNotificationService):
        service = SystemNotificationService()
        if metadata is not None:
            metadata.system_notification_service = service
    service.register_agent_run()
    return service


def flush_system_notifications(
    service: SystemNotificationService,
) -> list[tuple[StreamEvent, UsageSnapshot | None]]:
    events = service.flush_events()
    return [(event, None) for event in events]
