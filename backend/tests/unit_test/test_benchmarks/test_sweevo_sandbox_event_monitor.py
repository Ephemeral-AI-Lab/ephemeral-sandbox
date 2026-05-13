from __future__ import annotations

import asyncio
import json
from pathlib import Path

from live_e2e.audit.bus import AuditEventBus
from live_e2e.audit.events import Event, EventType
from live_e2e.audit.legacy import LegacySandboxAuditSink
from live_e2e.audit.node_id import NodeId
from live_e2e.audit.recorder import AuditRecorder
from live_e2e.audit.stream_bridge import stream_bridge
from audit.base import AuditEvent, AuditNode
from sandbox.audit import events as sandbox_events
from message.stream_events import ToolExecutionCompleted


def test_stream_bridge_derives_sandbox_subsystem_events() -> None:
    bus = AuditEventBus()
    events: list[Event] = []
    bus.subscribe(events.append)
    bridge = stream_bridge(bus, task_center_run_id="run-1")

    asyncio.run(
        bridge(
            ToolExecutionCompleted(
                tool_name="shell",
                output="{}",
                is_error=False,
                tool_id="toolu_1",
                agent_name="executor",
                run_id="task-1",
                metadata={
                    "status": "ok",
                    "changed_paths": ["a.txt"],
                    "conflict_reason": None,
                    "timings": {
                        "command_exec.prepare_snapshot_s": 0.01,
                        "overlay.total_s": 0.02,
                        "occ.prepare.total_s": 0.03,
                        "occ.commit.publish_layer_s": 0.04,
                        "occ.commit.total_s": 0.05,
                        "occ.apply.total_s": 0.06,
                        "command_exec.occ_apply_s": 0.07,
                        "layer_stack.auto_squash.total_s": 0.08,
                        "layer_stack.auto_squash.depth_after": 32.0,
                    },
                },
            )
        )
    )

    event_types = {event.type for event in events}
    assert EventType.TOOL_CALL_COMPLETED in event_types
    assert EventType.SANDBOX_LAYER_STACK_LEASE_ACQUIRED in event_types
    assert EventType.SANDBOX_OVERLAY_EXECUTED in event_types
    assert EventType.SANDBOX_OCC_CHANGESET_RECEIVED in event_types
    assert EventType.SANDBOX_OCC_CHANGES_COMMITTED in event_types
    assert EventType.SANDBOX_LAYER_STACK_LAYER_CREATED in event_types
    assert EventType.SANDBOX_LAYER_STACK_LAYERS_SQUASHED in event_types


def test_stream_bridge_skips_metadata_derivation_when_sandbox_audit_emitted() -> None:
    bus = AuditEventBus()
    events: list[Event] = []
    bus.subscribe(events.append)
    bridge = stream_bridge(bus, task_center_run_id="run-1")

    asyncio.run(
        bridge(
            ToolExecutionCompleted(
                tool_name="shell",
                output="{}",
                is_error=False,
                tool_id="toolu_1",
                agent_name="executor",
                run_id="task-1",
                metadata={
                    "sandbox_audit_emitted": True,
                    "status": "ok",
                    "changed_paths": ["a.txt"],
                    "timings": {
                        "overlay.total_s": 0.02,
                        "occ.prepare.total_s": 0.03,
                        "occ.apply.total_s": 0.06,
                    },
                },
            )
        )
    )

    assert [event.type for event in events] == [EventType.TOOL_CALL_COMPLETED]


def test_stream_bridge_derives_sandbox_conflict_event() -> None:
    bus = AuditEventBus()
    events: list[Event] = []
    bus.subscribe(events.append)
    bridge = stream_bridge(bus, task_center_run_id="run-1")

    asyncio.run(
        bridge(
            ToolExecutionCompleted(
                tool_name="edit_file",
                output="{}",
                is_error=True,
                tool_id="toolu_conflict",
                agent_name="executor",
                run_id="task-1",
                metadata={
                    "status": "aborted_overlap",
                    "changed_paths": ["a.txt"],
                    "conflict_reason": "anchor not found",
                    "timings": {"api.edit.lease_acquire_s": 0.01},
                },
            )
        )
    )

    conflicts = [
        event for event in events if event.type is EventType.SANDBOX_CONFLICT_DETECTED
    ]
    assert len(conflicts) == 1
    assert conflicts[0].payload["conflict_reason"] == "anchor not found"


def test_audit_recorder_persists_sandbox_events(tmp_path: Path) -> None:
    bus = AuditEventBus()
    recorder = AuditRecorder(tmp_path / "run", task_center_run_id="run-1", bus=bus)
    recorder.start()
    try:
        bus.publish(
            Event(
                type=EventType.SANDBOX_OCC_CHANGES_COMMITTED,
                node=NodeId(task_center_run_id="run-1", tool_name="write_file"),
                payload={"tool_name": "write_file"},
            )
        )
    finally:
        recorder.dispose()

    rows = [
        json.loads(line)
        for line in (recorder.run_dir / "sandbox_events.jsonl")
        .read_text(encoding="utf-8")
        .splitlines()
        if line.strip()
    ]
    assert rows[0]["event_type"] == "sandbox_occ_changes_committed"
    assert rows[0]["payload"]["tool_name"] == "write_file"


def test_legacy_sandbox_audit_sink_maps_namespaced_events_once(tmp_path: Path) -> None:
    bus = AuditEventBus()
    recorder = AuditRecorder(tmp_path / "run", task_center_run_id="run-1", bus=bus)
    sink = LegacySandboxAuditSink(bus)
    recorder.start()
    try:
        sink.publish(
            AuditEvent(
                source="sandbox",
                type=sandbox_events.OCC_COMMITTED,
                node=AuditNode(
                    task_center_run_id="run-1",
                    task_center_task_id="task-1",
                    tool_name="write_file",
                    tool_id="toolu_1",
                ),
                payload={
                    "operation": "write_file",
                    "status": "ok",
                    "changed_paths": ["a.py"],
                    "timings": {"occ.apply.total_s": 0.01},
                },
            )
        )
    finally:
        recorder.dispose()

    rows = [
        json.loads(line)
        for line in (recorder.run_dir / "sandbox_events.jsonl")
        .read_text(encoding="utf-8")
        .splitlines()
        if line.strip()
    ]
    assert len(rows) == 1
    assert rows[0]["event_type"] == "sandbox_occ_changes_committed"
    assert rows[0]["node"]["agent_run_id"] == "task-1"
    assert rows[0]["payload"]["tool_name"] == "write_file"
    assert rows[0]["payload"]["tool_id"] == "toolu_1"
