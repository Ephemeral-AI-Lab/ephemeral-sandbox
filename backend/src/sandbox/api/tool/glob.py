"""Internal implementation for the public sandbox glob verb."""

from __future__ import annotations

from audit.base import AuditSink
from sandbox.api.tool.core.audit import audited_operation
from sandbox.api.tool.core.results import glob_result_from_daemon_response
from sandbox.api.protocol import SandboxTransport
from sandbox.api.timeouts import GLOB_TIMEOUT_S
from sandbox.api.transport import DAEMON_OP_GLOB, DaemonSandboxTransport
from sandbox._shared.models import GlobRequest, GlobResult


async def glob(
    sandbox_id: str,
    request: GlobRequest,
    *,
    audit_sink: AuditSink | None = None,
    transport: SandboxTransport | None = None,
) -> GlobResult:
    """Enumerate workspace paths matching ``request.pattern`` in the sandbox."""
    selected_transport = transport or DaemonSandboxTransport()

    async def _call() -> GlobResult:
        payload: dict[str, object] = {
            "pattern": request.pattern,
            "caller": request.caller.audit_fields(),
        }
        if request.path is not None:
            payload["path"] = request.path
        raw = await selected_transport.call(
            sandbox_id,
            DAEMON_OP_GLOB,
            payload,
            timeout=GLOB_TIMEOUT_S,
        )
        return glob_result_from_daemon_response(raw)

    return await audited_operation(
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
