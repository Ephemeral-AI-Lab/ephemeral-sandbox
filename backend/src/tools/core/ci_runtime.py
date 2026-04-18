"""Shared code-intelligence runtime helpers used across toolkits.

Mutation-capable tools route process execution through
:func:`exec_ci_process_operation`, allowing the CI service to audit one complete
``process.exec`` command as the tool operation.
"""

from __future__ import annotations

import inspect
from dataclasses import dataclass
from typing import Any

from tools.core.base import ToolExecutionContext, ToolResult


@dataclass(frozen=True)
class _ProcessExecutionContext:
    description: str
    agent_id: str
    team_run_id: str
    agent_run_id: str
    task_id: str
    attribute_changes: bool


def get_ci_service(context: ToolExecutionContext) -> Any | None:
    """Get the CodeIntelligenceService from context, or None if unavailable."""
    return context.metadata.get("ci_service")


def ci_required_result(tool_name: str, detail: str) -> ToolResult:
    """Build a consistent error for tools that require code intelligence."""
    suffix = str(detail or "").strip()
    return ToolResult(
        output=(
            f"{tool_name}: Code intelligence service is unavailable."
            f"{' ' + suffix if suffix else ''}"
        ),
        is_error=True,
        metadata={"ci_required": True},
    )


def ci_write_required_result(
    tool_name: str,
    file_path: str,
    *,
    conflict: bool = False,
) -> ToolResult:
    """Build a consistent CI-required file-write error."""
    metadata = {"ci_required": True}
    if conflict:
        metadata["conflict"] = True
    operation = "Write" if "write" in tool_name else "Edit"
    return ToolResult(
        output=(
            f"{tool_name}: Code intelligence service is unavailable. "
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
    attribute_changes: bool = True,
) -> Any:
    """Run one process command through the audited CI execution boundary.

    Daytona tools describe the logical operation here. This helper owns the
    process execution context, then delegates execution to the CI service.
    """
    svc = get_ci_service(context)
    if svc is None:
        raise RuntimeError("Code intelligence service is unavailable")

    process_context = _build_process_execution_context(
        context,
        description=description,
        attribute_changes=attribute_changes,
    )
    return await _execute_ci_process_operation(
        svc,
        sandbox,
        command,
        timeout=timeout,
        process_context=process_context,
    )


def _build_process_execution_context(
    context: ToolExecutionContext,
    *,
    description: str,
    attribute_changes: bool,
) -> _ProcessExecutionContext:
    return _ProcessExecutionContext(
        description=description,
        agent_id=_resolved_agent_id(context),
        team_run_id=str(context.metadata.get("team_run_id") or ""),
        agent_run_id=str(context.metadata.get("agent_run_id") or ""),
        task_id=str(context.metadata.get("work_item_id") or ""),
        attribute_changes=attribute_changes,
    )


async def _execute_ci_process_operation(
    svc: Any,
    sandbox: Any,
    command: str,
    *,
    timeout: int | None,
    process_context: _ProcessExecutionContext,
) -> Any:
    audited_exec_descriptor = inspect.getattr_static(svc, "exec_process_operation", None)
    audited_exec = (
        getattr(svc, "exec_process_operation", None)
        if audited_exec_descriptor is not None
        else None
    )
    if not callable(audited_exec):
        raise RuntimeError("Code intelligence service exec_process_operation is unavailable")
    if not inspect.iscoroutinefunction(audited_exec):
        raise RuntimeError("Code intelligence service exec_process_operation must be async")
    return await audited_exec(
        sandbox,
        command,
        timeout=timeout,
        description=process_context.description,
        agent_id=process_context.agent_id,
        team_run_id=process_context.team_run_id,
        agent_run_id=process_context.agent_run_id,
        task_id=process_context.task_id,
        attribute_changes=process_context.attribute_changes,
    )


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
    "ci_write_required_result",
    "exec_ci_process_operation",
    "get_ci_service",
]
