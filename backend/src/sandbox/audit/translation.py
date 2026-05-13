"""Translate public sandbox operation results into audit events."""

from __future__ import annotations

from collections.abc import Mapping
from typing import Any, Literal

from audit.base import AuditEvent, AuditNode, JsonValue
from sandbox.audit import events
from sandbox.models import GuardedResultBase, SandboxCaller, SandboxResultBase

SandboxOperation = Literal[
    "read_file",
    "write_file",
    "edit_file",
    "shell",
    "raw_exec",
    "plugin",
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
    terminal_type = _terminal_type(payload["status"])
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
    return AuditEvent(
        source="sandbox",
        type=events.OPERATION_FAILED,
        node=node_from_caller(sandbox_id=sandbox_id, operation=operation, caller=caller),
        payload={
            "operation": operation,
            "status": "error",
            "changed_paths": [],
            "conflict_reason": None,
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
        mission_id=_none_if_empty(caller.task_center_mission_id),
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
        "timings": dict(result.timings),
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


def _terminal_type(status: object) -> str:
    if status == "conflict":
        return events.OPERATION_CONFLICTED
    if status == "error":
        return events.OPERATION_FAILED
    return events.OPERATION_COMPLETED


def _subsystem_events(
    *,
    node: AuditNode,
    payload: Mapping[str, Any],
) -> list[AuditEvent]:
    timings = payload.get("timings")
    if not isinstance(timings, dict) or not timings:
        return []

    status = payload.get("status")
    emitted: list[AuditEvent] = []
    if _has_timing(timings, "occ.prepare."):
        emitted.append(_subsystem_event(events.OCC_PREPARED, node, payload))
    if _has_timing(timings, "occ.") and status == "conflict":
        emitted.append(_subsystem_event(events.OCC_CONFLICTED, node, payload))
    elif _has_any_timing(timings, ("occ.commit.", "occ.apply.")) and status == "ok":
        emitted.append(_subsystem_event(events.OCC_COMMITTED, node, payload))

    if _has_any_timing(timings, ("overlay.", "command_exec.")):
        emitted.append(_subsystem_event(events.OVERLAY_EXECUTED, node, payload))

    if _has_any_timing(
        timings,
        (
            "layer_stack.lease_",
            "layer_stack.transaction_lock_wait",
            "layer_stack.transaction_lock_held",
        ),
    ):
        emitted.append(_subsystem_event(events.LAYER_STACK_LEASE_ACQUIRED, node, payload))
    if _has_any_timing(timings, ("layer_stack.publish", "layer_stack.layer_")):
        emitted.append(_subsystem_event(events.LAYER_STACK_LAYER_PUBLISHED, node, payload))
    if _has_auto_squash_fact(timings, payload):
        emitted.append(_subsystem_event(events.LAYER_STACK_AUTO_SQUASHED, node, payload))
    return emitted


def _subsystem_event(
    event_type: str,
    node: AuditNode,
    payload: Mapping[str, Any],
) -> AuditEvent:
    return AuditEvent(
        source="sandbox",
        type=event_type,
        node=node,
        payload=dict(payload),
    )


def _has_timing(timings: Mapping[object, object], prefix: str) -> bool:
    return any(str(key).startswith(prefix) for key in timings)


def _has_any_timing(timings: Mapping[object, object], prefixes: tuple[str, ...]) -> bool:
    return any(_has_timing(timings, prefix) for prefix in prefixes)


def _has_auto_squash_fact(
    timings: Mapping[object, object],
    payload: Mapping[str, Any],
) -> bool:
    if any("auto_squash" in str(key) for key in timings):
        return True
    return any("auto_squash" in str(key) for key in payload)


def _none_if_empty(value: str | None) -> str | None:
    if value is None:
        return None
    stripped = value.strip()
    return stripped or None


__all__ = [
    "SandboxOperation",
    "events_from_result",
    "failed_event",
    "node_from_caller",
    "operation_payload",
    "started_event",
]
