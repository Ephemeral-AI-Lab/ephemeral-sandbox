"""Translate public sandbox operation results into audit events."""

from __future__ import annotations

from collections.abc import Mapping
from typing import Any, Literal

from audit.base import AuditEvent, AuditNode, JsonValue

from sandbox.audit.conflict_markers import ALL_CONFLICT_MARKERS
from sandbox.audit import events
from sandbox._shared.models import GuardedResultBase, SandboxCaller, SandboxResultBase
from sandbox._shared.clock import normalize_timing_map
from sandbox.audit.timing import timing_audit_signals

SandboxOperation = Literal[
    "read_file",
    "write_file",
    "edit_file",
    "shell",
    "raw_exec",
    "plugin",
    "glob",
    "grep",
]


def started_event(
    *,
    sandbox_id: str,
    operation: SandboxOperation,
    caller: SandboxCaller | None,
    payload: Mapping[str, JsonValue] | None = None,
) -> AuditEvent:
    return AuditEvent(
        source="sandbox",
        type=events.OPERATION_STARTED,
        node=node_from_caller(sandbox_id=sandbox_id, operation=operation, caller=caller),
        payload={"operation": operation, **dict(payload or {})},
    )


def events_from_result(
    *,
    sandbox_id: str,
    operation: SandboxOperation,
    caller: SandboxCaller | None,
    result: SandboxResultBase,
) -> list[AuditEvent]:
    node = node_from_caller(
        sandbox_id=sandbox_id,
        operation=operation,
        caller=caller,
    )
    payload = operation_payload(operation=operation, result=result)
    terminal_type = events.OPERATION_COMPLETED
    if payload["status"] == "conflict":
        terminal_type = events.OPERATION_CONFLICTED
    elif payload["status"] == "error":
        terminal_type = events.OPERATION_FAILED
    emitted = [
        AuditEvent(
            source="sandbox",
            type=terminal_type,
            node=node,
            payload=payload,
        )
    ]
    emitted.extend(_subsystem_events(node=node, payload=payload))
    return emitted


def failed_event(
    *,
    sandbox_id: str,
    operation: SandboxOperation,
    caller: SandboxCaller | None,
    error: BaseException,
) -> AuditEvent:
    conflict_reason = _conflict_reason_from_error(error)
    status = "conflict" if conflict_reason is not None else "error"
    return AuditEvent(
        source="sandbox",
        type=(
            events.OPERATION_CONFLICTED
            if conflict_reason is not None
            else events.OPERATION_FAILED
        ),
        node=node_from_caller(sandbox_id=sandbox_id, operation=operation, caller=caller),
        payload={
            "operation": operation,
            "status": status,
            "changed_paths": [],
            "conflict_reason": conflict_reason,
            "warnings": [],
            "timings": {},
            "error_kind": type(error).__name__,
            "error": str(error),
        },
    )


def node_from_caller(
    *,
    sandbox_id: str,
    operation: SandboxOperation,
    caller: SandboxCaller | None,
) -> AuditNode:
    if caller is None:
        return AuditNode(sandbox_id=sandbox_id, tool_name=operation)
    return AuditNode(
        task_center_run_id=_none_if_empty(caller.task_center_run_id or caller.run_id),
        request_id=_none_if_empty(caller.task_center_request_id),
        goal_id=_none_if_empty(caller.task_center_goal_id),
        attempt_id=_none_if_empty(caller.task_center_attempt_id),
        task_center_task_id=_none_if_empty(
            caller.task_center_task_id or caller.task_id
        ),
        agent_name=_none_if_empty(caller.agent_id),
        agent_run_id=_none_if_empty(caller.agent_run_id),
        sandbox_id=sandbox_id,
        tool_name=_none_if_empty(caller.tool_name) or operation,
        tool_id=_none_if_empty(caller.tool_id),
    )


def operation_payload(
    *,
    operation: SandboxOperation,
    result: SandboxResultBase,
) -> dict[str, Any]:
    status = _status_from_result(result)
    return {
        "operation": operation,
        "status": status,
        "changed_paths": list(getattr(result, "changed_paths", ()) or ()),
        "conflict_reason": getattr(result, "conflict_reason", None),
        "warnings": list(getattr(result, "warnings", ()) or ()),
        "timings": normalize_timing_map(result.timings),
    }


def _status_from_result(result: SandboxResultBase) -> str:
    result_status = str(getattr(result, "status", "") or "")
    if (
        isinstance(result, GuardedResultBase)
        and result_status != "error"
        and (result.conflict is not None or result.conflict_reason)
    ):
        return "conflict" if not result.success else "ok"
    exit_code = getattr(result, "exit_code", 0)
    if not result.success or exit_code not in (0, "0"):
        return "error"
    return "ok"


def _subsystem_events(
    *,
    node: AuditNode,
    payload: Mapping[str, Any],
) -> list[AuditEvent]:
    timings = payload.get("timings")
    if not isinstance(timings, dict) or not timings:
        return []

    return [
        AuditEvent(
            source="sandbox",
            type=_EVENT_BY_SIGNAL[signal],
            node=node,
            payload=dict(payload),
        )
        for signal in timing_audit_signals(
            timings,
            status=payload.get("status"),
            payload=payload,
        )
    ]


def _none_if_empty(value: str | None) -> str | None:
    if value is None:
        return None
    stripped = value.strip()
    return stripped or None


def _conflict_reason_from_error(error: BaseException) -> str | None:
    message = str(getattr(error, "message", "") or error)
    lowered = message.lower()
    if any(marker in lowered for marker in ALL_CONFLICT_MARKERS):
        return message
    return None


_EVENT_BY_SIGNAL = {
    "occ_prepared": events.OCC_PREPARED,
    "occ_committed": events.OCC_COMMITTED,
    "occ_conflicted": events.OCC_CONFLICTED,
    "overlay_executed": events.OVERLAY_EXECUTED,
    "layer_stack_lease_acquired": events.LAYER_STACK_LEASE_ACQUIRED,
    "layer_stack_layer_published": events.LAYER_STACK_LAYER_PUBLISHED,
    "layer_stack_auto_squashed": events.LAYER_STACK_AUTO_SQUASHED,
    "resource_snapshot": events.RESOURCE_SNAPSHOT,
}


__all__ = [
    "SandboxOperation",
    "events_from_result",
    "failed_event",
    "node_from_caller",
    "operation_payload",
    "started_event",
]
