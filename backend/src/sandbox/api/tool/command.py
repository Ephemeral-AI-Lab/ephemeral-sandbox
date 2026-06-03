"""Internal implementation for Phase 3T command-session verbs."""

from __future__ import annotations

from collections.abc import Mapping
from typing import Any

from audit.base import AuditSink
from sandbox.api.tool._daemon_response_parsing import (
    daemon_request_identity_fields,
    parse_timing_map_field,
)
from sandbox.api.tool._operation_audit import run_audited_operation
from sandbox.api.timeouts import shell_dispatch_timeout
from sandbox.api.transport import (
    DAEMON_OP_COMMAND_CANCEL,
    DAEMON_OP_COMMAND_COLLECT_COMPLETED,
    DAEMON_OP_COMMAND_WRITE_STDIN,
    DAEMON_OP_EXEC_COMMAND,
    SandboxTransport,
    call_sandbox_daemon,
)
from sandbox._shared.clock import monotonic_now
from sandbox._shared.models import (
    CommandOutput,
    CommandSessionCancelRequest,
    CommandSessionWriteRequest,
    ExecCommandRequest,
    ExecCommandResult,
)


async def exec_command(
    sandbox_id: str,
    request: ExecCommandRequest,
    *,
    audit_sink: AuditSink | None = None,
    transport: SandboxTransport | None = None,
) -> ExecCommandResult:
    """Run or start a managed command session."""
    total_start = monotonic_now()

    async def _call() -> ExecCommandResult:
        payload = daemon_request_identity_fields(request) | {
            "cmd": request.cmd,
        }
        if request.yield_time_ms is not None:
            payload["yield_time_ms"] = request.yield_time_ms
        if request.timeout is not None:
            payload["timeout"] = request.timeout
        if request.max_output_tokens is not None:
            payload["max_output_tokens"] = request.max_output_tokens
        response = await call_sandbox_daemon(
            sandbox_id,
            DAEMON_OP_EXEC_COMMAND,
            payload,
            timeout=shell_dispatch_timeout(request.timeout),
            transport=transport,
        )
        timings = parse_timing_map_field(response.get("timings"))
        timings["api.exec_command.dispatch_total_s"] = monotonic_now() - total_start
        return _parse_exec_command_result(response, timings=timings)

    return await run_audited_operation(
        audit_sink=audit_sink,
        sandbox_id=sandbox_id,
        operation="exec_command",
        caller=request.caller,
        payload={},
        call=_call,
    )


async def write_stdin(
    sandbox_id: str,
    request: CommandSessionWriteRequest,
    *,
    transport: SandboxTransport | None = None,
) -> ExecCommandResult:
    payload = daemon_request_identity_fields(request) | {
        "command_session_id": request.command_session_id,
        "chars": request.chars,
    }
    if request.yield_time_ms is not None:
        payload["yield_time_ms"] = request.yield_time_ms
    if request.max_output_tokens is not None:
        payload["max_output_tokens"] = request.max_output_tokens
    response = await call_sandbox_daemon(
        sandbox_id,
        DAEMON_OP_COMMAND_WRITE_STDIN,
        payload,
        timeout=shell_dispatch_timeout(None),
        transport=transport,
    )
    return _parse_exec_command_result(response)


async def cancel_command_session(
    sandbox_id: str,
    request: CommandSessionCancelRequest,
    *,
    transport: SandboxTransport | None = None,
) -> ExecCommandResult:
    payload = daemon_request_identity_fields(request) | {
        "command_session_id": request.command_session_id,
    }
    response = await call_sandbox_daemon(
        sandbox_id,
        DAEMON_OP_COMMAND_CANCEL,
        payload,
        timeout=shell_dispatch_timeout(None),
        transport=transport,
    )
    return _parse_exec_command_result(response)


async def collect_command_completions(
    sandbox_id: str,
    *,
    agent_id: str,
    command_session_ids: list[str],
    transport: SandboxTransport | None = None,
) -> list[dict[str, Any]]:
    response = await call_sandbox_daemon(
        sandbox_id,
        DAEMON_OP_COMMAND_COLLECT_COMPLETED,
        {
            "agent_id": agent_id,
            "command_session_ids": command_session_ids,
        },
        timeout=shell_dispatch_timeout(None),
        transport=transport,
    )
    raw = response.get("completions")
    return [dict(item) for item in raw if isinstance(item, Mapping)] if isinstance(raw, list) else []


def _parse_exec_command_result(
    response: Mapping[str, Any],
    *,
    timings: dict[str, float] | None = None,
) -> ExecCommandResult:
    output_raw = response.get("output")
    output = output_raw if isinstance(output_raw, Mapping) else {}
    exit_code_raw = response.get("exit_code")
    exit_code = int(exit_code_raw) if isinstance(exit_code_raw, int) else None
    changed_paths_raw = response.get("changed_paths")
    changed_path_kinds_raw = response.get("changed_path_kinds")
    conflict_reason_raw = response.get("conflict_reason")
    mutation_source_raw = response.get("mutation_source")
    return ExecCommandResult(
        success=str(response.get("status") or "") not in {"error", "timed_out"},
        status=str(response.get("status") or "error"),
        exit_code=exit_code,
        output=CommandOutput(
            stdout=str(output.get("stdout") or ""),
            stderr=str(output.get("stderr") or ""),
        ),
        command_session_id=(
            str(response.get("command_session_id"))
            if response.get("command_session_id")
            else None
        ),
        timings=timings or parse_timing_map_field(response.get("timings")),
        conflict_reason=(
            str(conflict_reason_raw) if conflict_reason_raw is not None else None
        ),
        changed_paths=(
            [str(path) for path in changed_paths_raw]
            if isinstance(changed_paths_raw, list)
            else []
        ),
        changed_path_kinds=(
            {str(key): str(value) for key, value in changed_path_kinds_raw.items()}
            if isinstance(changed_path_kinds_raw, Mapping)
            else {}
        ),
        mutation_source=(
            str(mutation_source_raw) if mutation_source_raw is not None else ""
        ),
        error=response.get("error") if isinstance(response.get("error"), dict) else None,
    )


__all__ = [
    "cancel_command_session",
    "collect_command_completions",
    "exec_command",
    "write_stdin",
]
