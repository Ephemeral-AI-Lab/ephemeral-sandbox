"""Tests for the shared audit event bus."""

from __future__ import annotations

from audit.base import AuditEvent, AuditNode
from audit.bus import AuditEventBus


def test_audit_event_bus_fans_out_in_subscription_order() -> None:
    bus = AuditEventBus()
    event = AuditEvent(
        source="task_center",
        type="task_center.task.ready",
        node=AuditNode(task_center_task_id="task-1"),
    )
    calls: list[str] = []

    bus.subscribe(lambda received: calls.append(f"first:{received.type}"))
    bus.subscribe(lambda received: calls.append(f"second:{received.source}"))

    bus.publish(event)

    assert calls == ["first:task_center.task.ready", "second:task_center"]
    assert bus.errors == []


def test_audit_event_bus_unsubscribe_removes_handler() -> None:
    bus = AuditEventBus()
    event = AuditEvent(
        source="live_e2e",
        type="live_e2e.scenario.started",
        node=AuditNode(),
    )
    calls = 0

    def handler(received: AuditEvent) -> None:
        nonlocal calls
        assert received is event
        calls += 1

    unsubscribe = bus.subscribe(handler)
    bus.publish(event)
    unsubscribe()
    bus.publish(event)

    assert calls == 1


def test_audit_event_bus_captures_handler_errors() -> None:
    bus = AuditEventBus()
    event = AuditEvent(
        source="engine",
        type="engine.tool.failed",
        node=AuditNode(tool_name="shell"),
    )
    calls: list[str] = []

    def failing_handler(received: AuditEvent) -> None:
        calls.append(received.type)
        raise RuntimeError("boom")

    bus.subscribe(failing_handler)
    bus.subscribe(lambda received: calls.append(f"after:{received.type}"))

    bus.publish(event)

    assert calls == ["engine.tool.failed", "after:engine.tool.failed"]
    assert len(bus.errors) == 1
    assert bus.errors[0].event is event
    assert isinstance(bus.errors[0].error, RuntimeError)
