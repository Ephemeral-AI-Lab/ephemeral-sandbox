"""Compatibility mapping from namespaced audit events to legacy live-e2e events."""

from __future__ import annotations

from collections.abc import Iterable
from typing import Any

from audit.base import AuditEvent, AuditSink
from live_e2e.audit.bus import AuditEventBus
from live_e2e.audit.events import Event, EventType
from live_e2e.audit.node_id import NodeId
from sandbox.audit import events as sandbox_events


_SANDBOX_EVENT_MAP = {
    sandbox_events.OPERATION_CONFLICTED: EventType.SANDBOX_CONFLICT_DETECTED,
    sandbox_events.OCC_PREPARED: EventType.SANDBOX_OCC_CHANGESET_RECEIVED,
    sandbox_events.OCC_COMMITTED: EventType.SANDBOX_OCC_CHANGES_COMMITTED,
    sandbox_events.OCC_CONFLICTED: EventType.SANDBOX_CONFLICT_DETECTED,
    sandbox_events.OVERLAY_EXECUTED: EventType.SANDBOX_OVERLAY_EXECUTED,
    sandbox_events.LAYER_STACK_LEASE_ACQUIRED: (
        EventType.SANDBOX_LAYER_STACK_LEASE_ACQUIRED
    ),
    sandbox_events.LAYER_STACK_LAYER_PUBLISHED: (
        EventType.SANDBOX_LAYER_STACK_LAYER_CREATED
    ),
    sandbox_events.LAYER_STACK_AUTO_SQUASHED: (
        EventType.SANDBOX_LAYER_STACK_LAYERS_SQUASHED
    ),
}


class LegacySandboxAuditSink(AuditSink):
    """Forward sandbox-owned audit events into the legacy live-e2e bus."""

    def __init__(self, bus: AuditEventBus) -> None:
        self._bus = bus

    def publish(self, event: AuditEvent) -> None:
        for legacy_event in legacy_events_from_audit_event(event):
            self._bus.publish(legacy_event)


def legacy_events_from_audit_event(event: AuditEvent) -> tuple[Event, ...]:
    """Return legacy live-e2e events for one namespaced audit event."""
    if event.source != "sandbox":
        return ()
    legacy_type = _SANDBOX_EVENT_MAP.get(event.type)
    if legacy_type is None:
        return ()
    return (
        Event(
            type=legacy_type,
            node=_legacy_node(event),
            payload=_legacy_payload(event),
            correlation_id=event.correlation_id,
            ts=event.ts,
        ),
    )


def _legacy_node(event: AuditEvent) -> NodeId:
    node = event.node
    return NodeId(
        task_center_run_id=node.task_center_run_id or "",
        mission_id=node.mission_id,
        episode_id=node.episode_id,
        attempt_id=node.attempt_id,
        agent_name=node.agent_name,
        agent_run_id=node.agent_run_id or node.task_center_task_id,
        tool_name=node.tool_name,
    )


def _legacy_payload(event: AuditEvent) -> dict[str, Any]:
    payload = dict(event.payload)
    if "tool_name" not in payload and event.node.tool_name:
        payload["tool_name"] = event.node.tool_name
    if "tool_id" not in payload and event.node.tool_id:
        payload["tool_id"] = event.node.tool_id
    changed_paths = payload.get("changed_paths")
    if isinstance(changed_paths, Iterable) and not isinstance(
        changed_paths, (str, bytes, dict)
    ):
        payload["changed_paths"] = [str(path) for path in changed_paths]
    return payload


__all__ = ["LegacySandboxAuditSink", "legacy_events_from_audit_event"]
