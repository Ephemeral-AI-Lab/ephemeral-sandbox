"""Tests for shared audit base types."""

from __future__ import annotations

from dataclasses import asdict, is_dataclass
from datetime import UTC

from audit.base import AuditEvent, AuditNode, NoopAuditSink


def test_audit_event_defaults_are_serializable_with_dataclass_projection() -> None:
    event = AuditEvent(
        source="sandbox",
        type="sandbox.operation.completed",
        node=AuditNode(
            task_center_run_id="run-1",
            task_center_task_id="task-1",
            sandbox_id="sb-1",
            tool_name="edit_file",
            tool_id="tool-1",
        ),
        payload={"operation": "edit_file", "status": "ok"},
    )

    projected = asdict(event)

    assert projected["source"] == "sandbox"
    assert projected["type"] == "sandbox.operation.completed"
    assert projected["node"]["task_center_run_id"] == "run-1"
    assert projected["node"]["sandbox_id"] == "sb-1"
    assert projected["payload"] == {"operation": "edit_file", "status": "ok"}
    assert event.ts.tzinfo is UTC


def test_noop_audit_sink_accepts_events() -> None:
    event = AuditEvent(
        source="engine",
        type="engine.tool.started",
        node=AuditNode(agent_run_id="agent-run-1"),
    )

    assert is_dataclass(event)
    NoopAuditSink().publish(event)
