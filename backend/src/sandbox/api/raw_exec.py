"""Provider-adapter implementation for the unguarded raw exec primitive."""

from __future__ import annotations

from audit.base import AuditSink
from sandbox.api.tool._operation_audit import run_audited_operation
from sandbox._shared.models import RawExecResult
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

    async def _call() -> RawExecResult:
        return await get_adapter(sandbox_id).exec(
            sandbox_id,
            command,
            cwd=cwd,
            timeout=timeout,
        )

    return await run_audited_operation(
        audit_sink=audit_sink,
        sandbox_id=sandbox_id,
        operation="raw_exec",
        caller=None,
        payload={"cwd": cwd or ""},
        call=_call,
    )


__all__ = ["raw_exec"]
