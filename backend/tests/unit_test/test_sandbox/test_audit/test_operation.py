"""Tests for sandbox-owned audit event translation."""

from __future__ import annotations

from typing import get_args

from sandbox.audit import events
from sandbox.audit.timing import TimingAuditSignal
from sandbox.audit.translation import (
    FAILED_OPERATION_PAYLOAD_FIELDS,
    OPERATION_PAYLOAD_FIELDS,
    SUPPORTED_OPERATIONS,
    SandboxOperation,
    events_from_result,
    node_from_caller,
)
from sandbox.shared.models import ConflictInfo, SandboxCaller, WriteFileResult


def test_event_families_group_all_known_event_types_once() -> None:
    flattened = [
        event_type
        for family_events in events.EVENT_FAMILIES.values()
        for event_type in family_events
    ]

    assert flattened == list(events.ALL_EVENT_TYPES)
    assert len(flattened) == len(set(flattened))
    assert events.EVENT_FAMILIES == {
        "operation": events.OPERATION_EVENTS,
        "occ": events.OCC_EVENTS,
        "overlay": events.OVERLAY_EVENTS,
        "layer_stack": events.LAYER_STACK_EVENTS,
        "resource": events.RESOURCE_EVENTS,
        "workspace_lifecycle": events.WORKSPACE_LIFECYCLE_EVENTS,
        "isolated_workspace": events.ISOLATED_WORKSPACE_EVENTS,
    }


def test_timing_signal_mapping_covers_every_timing_signal() -> None:
    assert set(events.TIMING_SIGNAL_EVENTS) == set(get_args(TimingAuditSignal))


def test_operation_catalog_covers_payload_and_operation_types() -> None:
    assert SUPPORTED_OPERATIONS == get_args(SandboxOperation)
    assert OPERATION_PAYLOAD_FIELDS == (
        "operation",
        "status",
        "changed_paths",
        "changed_path_kinds",
        "mutation_source",
        "conflict_reason",
        "warnings",
        "timings",
        "error",
    )
    assert FAILED_OPERATION_PAYLOAD_FIELDS == OPERATION_PAYLOAD_FIELDS + (
        "error_kind",
    )


def test_node_from_caller_uses_task_center_fields_before_legacy_run_id() -> None:
    node = node_from_caller(
        sandbox_id="sb-1",
        operation="edit_file",
        caller=SandboxCaller(
            agent_id="agent-1",
            run_id="legacy-run",
            agent_run_id="agent-run-1",
            task_id="legacy-task",
            task_center_run_id="tc-run",
            task_center_task_id="tc-task",
            task_center_attempt_id="attempt-1",
            task_center_workflow_id="goal-1",
            task_center_request_id="request-1",
            tool_id="tool-1",
        ),
    )

    assert node.task_center_run_id == "tc-run"
    assert node.task_center_task_id == "tc-task"
    assert node.attempt_id == "attempt-1"
    assert node.workflow_id == "goal-1"
    assert node.request_id == "request-1"
    assert node.agent_name == "agent-1"
    assert node.agent_run_id == "agent-run-1"
    assert node.sandbox_id == "sb-1"
    assert node.tool_name == "edit_file"
    assert node.tool_use_id == "tool-1"


def test_events_from_result_emits_one_terminal_operation_event_plus_subsystems() -> None:
    result = WriteFileResult(
        success=True,
        changed_paths=("a.py",),
        status="ok",
        timings={
            "occ.prepare.total_s": 0.01,
            "occ.apply.total_s": 0.02,
            "workspace.tool_s": 0.03,
            "layer_stack.lease_acquire_s": 0.04,
            "layer_stack.publish_s": 0.05,
            "layer_stack.auto_squash.total_s": 0.06,
        },
    )

    emitted = events_from_result(
        sandbox_id="sb-1",
        operation="write_file",
        caller=SandboxCaller(agent_id="agent-1"),
        result=result,
    )

    assert [event.type for event in emitted] == [
        events.OPERATION_COMPLETED,
        events.OCC_PREPARED,
        events.OCC_COMMITTED,
        events.OVERLAY_EXECUTED,
        events.LAYER_STACK_LEASE_ACQUIRED,
        events.LAYER_STACK_LAYER_PUBLISHED,
        events.LAYER_STACK_AUTO_SQUASHED,
    ]
    terminal = emitted[0]
    assert terminal.payload["operation"] == "write_file"
    assert terminal.payload["status"] == "ok"
    assert terminal.payload["changed_paths"] == ["a.py"]
    assert terminal.payload["timings"] == result.timings
    assert emitted[1].payload["timings"] == {"occ.prepare.total_s": 0.01}
    assert emitted[2].payload["timings"] == {"occ.apply.total_s": 0.02}
    assert emitted[3].payload["timings"] == {"workspace.tool_s": 0.03}
    assert emitted[4].payload["timings"] == {"layer_stack.lease_acquire_s": 0.04}
    assert emitted[5].payload["timings"] == {"layer_stack.publish_s": 0.05}
    assert emitted[6].payload["timings"] == {"layer_stack.auto_squash.total_s": 0.06}


def test_events_from_conflict_result_emits_conflict_operation_and_occ_conflict() -> None:
    result = WriteFileResult(
        success=False,
        changed_paths=(),
        status="aborted_version",
        conflict=ConflictInfo(
            reason="aborted_version",
            conflict_file="a.py",
            message="base mismatch",
        ),
        conflict_reason="base mismatch",
        timings={"occ.prepare.total_s": 0.01, "occ.apply.total_s": 0.02},
    )

    emitted = events_from_result(
        sandbox_id="sb-1",
        operation="write_file",
        caller=SandboxCaller(agent_id="agent-1"),
        result=result,
    )

    assert [event.type for event in emitted] == [
        events.OPERATION_CONFLICTED,
        events.OCC_PREPARED,
        events.OCC_CONFLICTED,
    ]
    assert emitted[0].payload["status"] == "conflict"
    assert emitted[0].payload["conflict_reason"] == "base mismatch"
