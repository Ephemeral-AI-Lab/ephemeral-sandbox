"""Tests for sandbox-owned audit event translation."""

from __future__ import annotations

from sandbox.audit import events
from sandbox.audit.translation import events_from_result, node_from_caller
from sandbox.models import ConflictInfo, SandboxCaller, WriteFileResult


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
            task_center_mission_id="mission-1",
            task_center_request_id="request-1",
            tool_id="tool-1",
        ),
    )

    assert node.task_center_run_id == "tc-run"
    assert node.task_center_task_id == "tc-task"
    assert node.attempt_id == "attempt-1"
    assert node.mission_id == "mission-1"
    assert node.request_id == "request-1"
    assert node.agent_name == "agent-1"
    assert node.agent_run_id == "agent-run-1"
    assert node.sandbox_id == "sb-1"
    assert node.tool_name == "edit_file"
    assert node.tool_id == "tool-1"


def test_events_from_result_emits_one_terminal_operation_event_plus_subsystems() -> None:
    result = WriteFileResult(
        success=True,
        changed_paths=("a.py",),
        status="ok",
        timings={
            "occ.prepare.total_s": 0.01,
            "occ.apply.total_s": 0.02,
            "overlay.run.total_s": 0.03,
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
