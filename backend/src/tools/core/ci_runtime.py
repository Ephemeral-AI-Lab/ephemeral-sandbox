"""Shared code-intelligence runtime helpers used across toolkits.

Mutation-capable tools route process execution through
:func:`exec_ci_process_operation`, allowing the CI service to audit one complete
``process.exec`` command as the tool operation.
"""

from __future__ import annotations

import inspect
from typing import Any

from tools.core.base import ToolExecutionContext, ToolResult


def get_ci_service(context: ToolExecutionContext) -> Any | None:
    """Get the CodeIntelligenceService from context, or None if unavailable."""
    return context.metadata.get("ci_service")


def ci_required_result(tool_name: str, detail: str) -> ToolResult:
    """Build a consistent error for tools that require CI/OCC."""
    suffix = str(detail or "").strip()
    return ToolResult(
        output=(
            f"{tool_name}: Code intelligence/OCC is unavailable."
            f"{' ' + suffix if suffix else ''}"
        ),
        is_error=True,
        metadata={"occ_required": True},
    )


def occ_required_result(
    tool_name: str,
    file_path: str,
    *,
    conflict: bool = False,
) -> ToolResult:
    """Build a consistent OCC-required file-write error."""
    metadata = {"occ_required": True}
    if conflict:
        metadata["conflict"] = True
    operation = "Write" if "write" in tool_name else "Edit"
    return ToolResult(
        output=(
            f"{tool_name}: Code intelligence/OCC is unavailable. "
            f"{operation} of {file_path} is disabled. Direct sandbox write fallback is disabled."
        ),
        is_error=True,
        metadata=metadata,
    )


async def exec_ci_process_operation(
    context: ToolExecutionContext,
    sandbox: Any,
    command: str,
    *,
    timeout: int | None = None,
    description: str,
    edit_type: str = "process",
) -> Any:
    """Run one process command through the OCC-aware execution entry point.

    CodeAct delegates command execution here; lower layers run the command
    and audit the complete process operation.
    """
    svc = get_ci_service(context)
    if svc is None:
        raise RuntimeError("Code intelligence/OCC is unavailable")

    audited_exec_descriptor = inspect.getattr_static(svc, "exec_process_operation", None)
    audited_exec = (
        getattr(svc, "exec_process_operation", None)
        if audited_exec_descriptor is not None
        else None
    )
    if callable(audited_exec):
        response = audited_exec(
            sandbox,
            command,
            timeout=timeout,
            description=description,
            edit_type=edit_type,
            agent_id=_resolved_agent_id(context),
            team_run_id=str(context.metadata.get("team_run_id") or ""),
            agent_run_id=str(context.metadata.get("agent_run_id") or ""),
            task_id=str(context.metadata.get("work_item_id") or ""),
        )
    else:
        process = getattr(sandbox, "process", None)
        exec_fn = getattr(process, "exec", None) if process is not None else None
        if not callable(exec_fn):
            raise RuntimeError("Sandbox process.exec is unavailable")
        response = exec_fn(command, timeout=timeout) if timeout is not None else exec_fn(command)
    if inspect.isawaitable(response):
        return await response
    return response


def _resolved_agent_id(context: ToolExecutionContext, *, preferred: str = "") -> str:
    agent_id = str(preferred or "").strip()
    if agent_id:
        return agent_id
    agent_name = str(context.metadata.get("agent_name") or "").strip()
    if agent_name:
        return agent_name
    return str(context.metadata.get("agent_run_id") or "").strip()


__all__ = [
    "ci_required_result",
    "exec_ci_process_operation",
    "get_ci_service",
    "occ_required_result",
]
