"""Shared audit wrapper for sandbox API tool verbs."""

from __future__ import annotations

from collections.abc import Awaitable, Callable, Mapping
from typing import TypeVar

from audit.base import AuditSink, JsonValue
from sandbox.audit.operation import (
    publish_operation_failed,
    publish_operation_result,
    publish_operation_started,
)
from sandbox.audit.translation import SandboxOperation
from sandbox._shared.models import SandboxCaller, SandboxResultBase

TResult = TypeVar("TResult", bound=SandboxResultBase)


async def audited_operation(
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
    publish_operation_started(
        audit_sink,
        sandbox_id=sandbox_id,
        operation=operation,
        caller=caller,
        payload=payload,
    )
    try:
        result = await call()
    except Exception as exc:
        conflict_result = (
            conflict_from_error(exc) if conflict_from_error is not None else None
        )
        if conflict_result is not None:
            # Recoverable: publish the conflict result and suppress the exception.
            publish_operation_result(
                audit_sink,
                sandbox_id=sandbox_id,
                operation=operation,
                caller=caller,
                result=conflict_result,
            )
            return conflict_result
        publish_operation_failed(
            audit_sink,
            sandbox_id=sandbox_id,
            operation=operation,
            caller=caller,
            error=exc,
        )
        raise
    publish_operation_result(
        audit_sink,
        sandbox_id=sandbox_id,
        operation=operation,
        caller=caller,
        result=result,
    )
    return result


__all__ = ["audited_operation"]
