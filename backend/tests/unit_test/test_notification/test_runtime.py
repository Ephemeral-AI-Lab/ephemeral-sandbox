"""Unit tests for notification runtime primitives."""

from __future__ import annotations

import pytest

from notification._runtime import SystemNotificationService


@pytest.mark.asyncio
async def test_pop_pending_notifications_preserves_events_for_agent_runs() -> None:
    service = SystemNotificationService()
    service.register_agent_run()

    await service.notify_system("keep stream event")

    blocks = service.pop_pending_notifications()
    assert [block.text for block in blocks] == ["keep stream event"]

    events = service.flush_events()
    assert [event.text for event in events] == ["keep stream event"]


@pytest.mark.asyncio
async def test_pop_pending_notifications_clears_events_for_standalone_tool_execution() -> None:
    service = SystemNotificationService()

    await service.notify_system("standalone note")

    blocks = service.pop_pending_notifications()
    assert [block.text for block in blocks] == ["standalone note"]
    assert service.flush_events() == []


@pytest.mark.asyncio
async def test_flush_events_reconstructs_event_when_emit_callback_consumed_stream_copy() -> None:
    emitted = []

    async def _emit(event) -> None:
        emitted.append(event.text)

    service = SystemNotificationService(emit=_emit)

    await service.notify_system("rebuild from transcript block")

    assert emitted == ["rebuild from transcript block"]
    events = service.flush_events()
    assert [event.text for event in events] == ["rebuild from transcript block"]
