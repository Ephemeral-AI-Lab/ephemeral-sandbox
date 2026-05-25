"""Internal implementation for the public sandbox glob verb."""

from __future__ import annotations

from audit.base import AuditSink
from sandbox.api.tool._daemon_requests import daemon_identity_payload
from sandbox.api.tool._operation_audit import run_audited_operation
from sandbox.api.tool._daemon_results import glob_result_from_daemon_response
from sandbox.api.timeouts import GLOB_TIMEOUT_S
from sandbox.api.transport import DAEMON_OP_GLOB, SandboxTransport, call_sandbox_daemon
from sandbox._shared.models import GlobRequest, GlobResult


async def glob(
    sandbox_id: str,
    request: GlobRequest,
    *,
    audit_sink: AuditSink | None = None,
    transport: SandboxTransport | None = None,
) -> GlobResult:
    """Enumerate workspace paths matching ``request.pattern`` in the sandbox."""

    async def _call() -> GlobResult:
        payload = daemon_identity_payload(request) | {"pattern": request.pattern}
        if request.path is not None:
            payload["path"] = request.path
        response = await call_sandbox_daemon(
            sandbox_id,
            DAEMON_OP_GLOB,
            payload,
            timeout=GLOB_TIMEOUT_S,
            transport=transport,
        )
        return glob_result_from_daemon_response(response)

    return await run_audited_operation(
        audit_sink=audit_sink,
        sandbox_id=sandbox_id,
        operation="glob",
        caller=request.caller,
        payload={
            "pattern": request.pattern,
            "path": request.path or "",
        },
        call=_call,
    )


__all__ = ["glob"]
