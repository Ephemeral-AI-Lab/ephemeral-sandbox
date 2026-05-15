"""Publish sandbox operation audit events from public API boundaries."""

from __future__ import annotations

from collections.abc import Mapping

from audit.base import AuditSink, JsonValue, NoopAuditSink
from sandbox.audit.translation import (
    SandboxOperation,
    events_from_result,
    failed_event,
    started_event,
)
from sandbox._shared.models import SandboxCaller, SandboxResultBase


def publish_operation_started(
    audit_sink: AuditSink | None,
    *,
    sandbox_id: str,
    operation: SandboxOperation,
    caller: SandboxCaller | None,
    payload: Mapping[str, JsonValue] | None = None,
) -> None:
    _sink(audit_sink).publish(
        started_event(
            sandbox_id=sandbox_id,
            operation=operation,
            caller=caller,
            payload=payload,
        )
    )


def publish_operation_result(
    audit_sink: AuditSink | None,
    *,
    sandbox_id: str,
    operation: SandboxOperation,
    caller: SandboxCaller | None,
    result: SandboxResultBase,
) -> None:
    sink = _sink(audit_sink)
    for event in events_from_result(
        sandbox_id=sandbox_id,
        operation=operation,
        caller=caller,
        result=result,
    ):
        sink.publish(event)


def publish_operation_failed(
    audit_sink: AuditSink | None,
    *,
    sandbox_id: str,
    operation: SandboxOperation,
    caller: SandboxCaller | None,
    error: BaseException,
) -> None:
    _sink(audit_sink).publish(
        failed_event(
            sandbox_id=sandbox_id,
            operation=operation,
            caller=caller,
            error=error,
        )
    )


def _sink(audit_sink: AuditSink | None) -> AuditSink:
    return audit_sink if audit_sink is not None else NoopAuditSink()


__all__ = [
    "publish_operation_failed",
    "publish_operation_result",
    "publish_operation_started",
]
