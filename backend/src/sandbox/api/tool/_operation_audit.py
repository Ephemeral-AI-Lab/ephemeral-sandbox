"""Audit event publication around one sandbox API operation."""

from __future__ import annotations

from collections.abc import Awaitable, Callable, Mapping
from typing import TypeVar

from audit.base import AuditEvent, AuditSink, JsonValue, NoopAuditSink
from sandbox.audit.translation import (
    SandboxOperation,
    events_from_result,
    failed_event,
    started_event,
)
from sandbox._shared.models import SandboxCaller, SandboxResultBase

TResult = TypeVar("TResult", bound=SandboxResultBase)


async def run_audited_operation(
    *,
    audit_sink: AuditSink | None,
    sandbox_id: str,
    operation: SandboxOperation,
    caller: SandboxCaller | None,
    payload: Mapping[str, JsonValue],
    call: Callable[[], Awaitable[TResult]],
    conflict_from_error: Callable[[BaseException], TResult | None] | None = None,
) -> TResult:
    """Publish start/result/failure events around one sandbox operation."""
    sink = audit_sink if audit_sink is not None else NoopAuditSink()

    def publish(event: AuditEvent) -> None:
        sink.publish(event)

    def publish_result(result: SandboxResultBase) -> None:
        for event in events_from_result(
            sandbox_id=sandbox_id,
            operation=operation,
            caller=caller,
            result=result,
        ):
            sink.publish(event)

    publish(
        started_event(
            sandbox_id=sandbox_id,
            operation=operation,
            caller=caller,
            payload=payload,
        )
    )
    try:
        result = await call()
    except Exception as exc:
        conflict_result = (
            conflict_from_error(exc) if conflict_from_error is not None else None
        )
        if conflict_result is not None:
            # Recoverable: publish the conflict result and suppress the exception.
            publish_result(conflict_result)
            return conflict_result
        publish(
            failed_event(
                sandbox_id=sandbox_id,
                operation=operation,
                caller=caller,
                error=exc,
            )
        )
        raise
    publish_result(result)
    return result
