"""Shared helpers for command-session tools."""

from __future__ import annotations

import json

from pydantic import BaseModel, Field

from sandbox._shared.clock import normalize_timing_map
from sandbox._shared.models import CommandOutput, ExecCommandResult
from tools._framework.core.base import ToolExecutionContextService, ToolResult


class CommandToolOutput(BaseModel):
    status: str
    exit_code: int | None
    output: dict[str, str]
    command_session_id: str | None = None
    stdout: str = ""
    stderr: str = ""
    changed_paths: list[str] = Field(default_factory=list)
    changed_path_kinds: dict[str, str] = Field(default_factory=dict)
    mutation_source: str = ""
    conflict_reason: str | None = None
    error: dict[str, object] | None = None


def command_result_payload(result: ExecCommandResult) -> dict[str, object]:
    payload = {
        "status": result.status,
        "exit_code": result.exit_code,
        "output": {
            "stdout": result.output.stdout,
            "stderr": result.output.stderr,
        },
        "stdout": result.output.stdout,
        "stderr": result.output.stderr,
        "changed_paths": list(result.changed_paths),
        "changed_path_kinds": dict(result.changed_path_kinds),
        "mutation_source": result.mutation_source,
        "conflict_reason": result.conflict_reason,
    }
    if result.command_session_id:
        payload["command_session_id"] = result.command_session_id
    if result.error:
        payload["error"] = dict(result.error)
    return payload


def command_tool_result(result: ExecCommandResult) -> ToolResult:
    payload = command_result_payload(result)
    metadata: dict[str, object] = {
        "status": result.status,
        "command_session_id": result.command_session_id or "",
        "changed_paths": list(result.changed_paths),
        "changed_path_kinds": dict(result.changed_path_kinds),
        "mutation_source": result.mutation_source,
        "conflict_reason": result.conflict_reason,
    }
    if result.timings:
        metadata["timings"] = normalize_timing_map(result.timings)
    return ToolResult(
        output=json.dumps(payload),
        is_error=result.status in {"error", "timed_out"},
        metadata=metadata,
    )


def recover_command_session_result_from_supervisor(
    context: ToolExecutionContextService,
    result: ExecCommandResult,
    *,
    command_session_id: str,
) -> ExecCommandResult:
    """Recover a terminal result already claimed by background notification polling."""
    if not _is_command_session_not_found(result):
        return result
    manager = context.get("background_task_manager")
    get_result = getattr(manager, "get_command_session_result", None)
    if not callable(get_result):
        return result
    stored = get_result(command_session_id)
    if not isinstance(stored, dict):
        return result
    return _exec_command_result_from_payload(
        stored,
        command_session_id=command_session_id,
        timings=result.timings,
    )


def mark_command_session_result_reported_by_tool(
    context: ToolExecutionContextService,
    result: ExecCommandResult,
    *,
    command_session_id: str | None = None,
) -> None:
    session_id = result.command_session_id or command_session_id
    if not session_id or result.status == "running":
        return
    if result.status == "error" and result.command_session_id is None:
        return
    manager = context.get("background_task_manager")
    mark = getattr(manager, "mark_command_session_result_reported_by_tool", None)
    if not callable(mark):
        return
    payload = command_result_payload(result)
    mark(
        command_session_id=session_id,
        result=payload,
    )


def _is_command_session_not_found(result: ExecCommandResult) -> bool:
    return (
        result.status == "error"
        and result.command_session_id is None
        and result.output.stderr == "command_session_not_found"
    )


def _exec_command_result_from_payload(
    payload: dict[str, object],
    *,
    command_session_id: str,
    timings: dict[str, float] | None,
) -> ExecCommandResult:
    output_raw = payload.get("output")
    output = output_raw if isinstance(output_raw, dict) else {}
    exit_code_raw = payload.get("exit_code")
    changed_paths_raw = payload.get("changed_paths")
    changed_path_kinds_raw = payload.get("changed_path_kinds")
    conflict_reason_raw = payload.get("conflict_reason")
    mutation_source_raw = payload.get("mutation_source")
    status = str(payload.get("status") or "error")
    return ExecCommandResult(
        success=status not in {"error", "timed_out"},
        status=status,
        exit_code=exit_code_raw if isinstance(exit_code_raw, int) else None,
        output=CommandOutput(
            stdout=str(output.get("stdout") or ""),
            stderr=str(output.get("stderr") or ""),
        ),
        command_session_id=str(payload.get("command_session_id") or command_session_id),
        timings=timings or {},
        changed_paths=(
            [str(path) for path in changed_paths_raw]
            if isinstance(changed_paths_raw, list)
            else []
        ),
        changed_path_kinds=(
            {str(key): str(value) for key, value in changed_path_kinds_raw.items()}
            if isinstance(changed_path_kinds_raw, dict)
            else {}
        ),
        mutation_source=(
            str(mutation_source_raw) if mutation_source_raw is not None else ""
        ),
        conflict_reason=(
            str(conflict_reason_raw) if conflict_reason_raw is not None else None
        ),
        error=payload.get("error") if isinstance(payload.get("error"), dict) else None,
    )


__all__ = [
    "CommandToolOutput",
    "command_result_payload",
    "command_tool_result",
    "mark_command_session_result_reported_by_tool",
    "recover_command_session_result_from_supervisor",
]
