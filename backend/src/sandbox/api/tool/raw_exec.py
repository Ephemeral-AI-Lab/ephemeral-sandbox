"""Un-guarded sandbox command execution.

This primitive is reserved for runtime setup, status/control, and debug paths.
Agent-visible shell execution must go through the guarded public verbs.
"""

from __future__ import annotations

from audit.base import AuditSink
from sandbox.audit.operation import (
    publish_operation_failed,
    publish_operation_result,
    publish_operation_started,
)
from sandbox.models import RawExecResult
from sandbox.provider.registry import get_adapter


async def raw_exec(
    sandbox_id: str,
    command: str,
    *,
    cwd: str | None = None,
    timeout: int | None = None,
    audit_sink: AuditSink | None = None,
) -> RawExecResult:
    """Run *command* through the registered provider adapter."""
    publish_operation_started(
        audit_sink,
        sandbox_id=sandbox_id,
        operation="raw_exec",
        caller=None,
        payload={"cwd": cwd or ""},
    )
    try:
        result = await get_adapter(sandbox_id).exec(
            sandbox_id,
            command,
            cwd=cwd,
            timeout=timeout,
        )
    except Exception as exc:
        publish_operation_failed(
            audit_sink,
            sandbox_id=sandbox_id,
            operation="raw_exec",
            caller=None,
            error=exc,
        )
        raise
    publish_operation_result(
        audit_sink,
        sandbox_id=sandbox_id,
        operation="raw_exec",
        caller=None,
        result=result,
    )
    return result


__all__ = ["raw_exec"]
