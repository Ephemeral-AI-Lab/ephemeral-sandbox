"""StreamEvent → audit Event translation."""

from __future__ import annotations

from collections.abc import Awaitable, Callable

from live_e2e.audit.bus import AuditEventBus
from live_e2e.audit.events import Event, EventType
from live_e2e.audit.node_id import NodeId
from live_e2e.audit.sandbox_events import (
    sandbox_events_from_tool_completion,
)
from message.stream_events import ToolExecutionCompleted, ToolExecutionStarted

__all__ = ["stream_bridge"]


def stream_bridge(
    bus: AuditEventBus,
    *,
    task_center_run_id: str,
) -> Callable[[object], Awaitable[None]]:
    """Return an async on_agent_event callable that translates StreamEvents to audit Events."""

    async def _on_event(stream_event: object) -> None:
        if isinstance(stream_event, ToolExecutionStarted):
            node = NodeId(
                task_center_run_id=task_center_run_id,
                agent_name=stream_event.agent_name or None,
                agent_run_id=stream_event.run_id or None,
                tool_name=stream_event.tool_name or None,
            )
            bus.publish(
                Event(
                    type=EventType.TOOL_CALL_STARTED,
                    node=node,
                    payload={
                        "tool_name": stream_event.tool_name,
                        "tool_input": stream_event.tool_input,
                        "tool_id": stream_event.tool_id,
                    },
                )
            )
        elif isinstance(stream_event, ToolExecutionCompleted):
            metadata = dict(stream_event.metadata or {})
            node = NodeId(
                task_center_run_id=task_center_run_id,
                agent_name=stream_event.agent_name or None,
                agent_run_id=stream_event.run_id or None,
                tool_name=stream_event.tool_name or None,
            )
            event_type = (
                EventType.TOOL_CALL_ERROR
                if stream_event.is_error
                else EventType.TOOL_CALL_COMPLETED
            )
            bus.publish(
                Event(
                    type=event_type,
                    node=node,
                    payload={
                        "tool_name": stream_event.tool_name,
                        "output": stream_event.output,
                        "is_error": stream_event.is_error,
                        "tool_id": stream_event.tool_id,
                        "metadata": metadata,
                        "does_terminate": stream_event.does_terminate,
                    },
                )
            )
            if not metadata.get("sandbox_audit_emitted"):
                for sandbox_event in sandbox_events_from_tool_completion(
                    stream_event,
                    task_center_run_id=task_center_run_id,
                ):
                    bus.publish(sandbox_event)
        # All other StreamEvent subtypes are silently ignored.

    return _on_event
