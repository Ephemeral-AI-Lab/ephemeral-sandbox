"""Provider-neutral tool-context helpers for sandbox-backed tools."""

from __future__ import annotations

from typing import Any

from sandbox._shared.models import SandboxCaller
from tools._framework.core.context import ToolExecutionContextService
from tools._framework.core.results import ToolResult


def sandbox_caller_from_tool_context(
    context: ToolExecutionContextService,
) -> SandboxCaller:
    """Build the sandbox caller identity for a tool call."""
    agent_run_id = str(context.agent_run_id or "")
    agent_id = agent_run_id.strip() or str(context.agent_name or "").strip()
    return SandboxCaller(
        agent_id=agent_id,
        run_id=str(context.get("run_id") or ""),
        agent_run_id=agent_run_id,
        task_id=str(context.get("task_id") or ""),
        request_id=str(context.get("request_id") or ""),
        attempt_id=str(context.get("attempt_id") or ""),
        workflow_id=str(context.get("workflow_id") or ""),
        tool_id=str(context.get("tool_use_id") or ""),
    )


def sandbox_audit_kwargs_from_tool_context(
    context: ToolExecutionContextService,
) -> dict[str, Any]:
    """Return sandbox API audit kwargs when an audit sink is available."""
    audit_sink = context.get("sandbox_audit_sink")
    if audit_sink is None:
        return {}
    publish = getattr(audit_sink, "publish", None)
    if not callable(publish):
        return {}
    return {"audit_sink": audit_sink}


def sandbox_audit_metadata_from_tool_context(
    context: ToolExecutionContextService,
) -> dict[str, bool]:
    """Mark tool metadata when sandbox audit events were emitted directly."""
    return (
        {"sandbox_audit_emitted": True}
        if sandbox_audit_kwargs_from_tool_context(context)
        else {}
    )


def sandbox_repo_root_from_tool_context(context: ToolExecutionContextService) -> str:
    """Return the sandbox repository root for tool output/path resolution."""
    return str(context.get("repo_root") or "").strip()


def resolve_tool_sandbox_path(path: str, context: ToolExecutionContextService) -> str:
    """Resolve a repo-relative path against the sandbox repository root."""
    # Trust boundary: absolute paths are passed through verbatim. The
    # sandbox provider (isolated rootfs) is the authoritative layer that
    # refuses or sandboxes host paths; this helper does not gate on them.
    if path.startswith("/"):
        return path
    repo_root = sandbox_repo_root_from_tool_context(context)
    if repo_root:
        return f"{repo_root.rstrip('/')}/{path}"
    return path


def sandbox_path_error_message(exc: Exception, path: str) -> str | None:
    """Return a user-facing path error when one can be recognized."""
    message = str(exc)
    if isinstance(exc, FileNotFoundError) or "No such file or directory" in message:
        return f"Path does not exist: {path}"
    return None


def sandbox_id_or_missing_error_result(
    context: ToolExecutionContextService,
) -> tuple[str, ToolResult | None]:
    sandbox_id = str(context.get("sandbox_id") or "").strip()
    if sandbox_id:
        return sandbox_id, None
    return "", ToolResult(
        output="Sandbox id is unavailable.",
        is_error=True,
        metadata={"sandbox_required": True},
    )


__all__ = [
    "sandbox_audit_kwargs_from_tool_context",
    "sandbox_audit_metadata_from_tool_context",
    "sandbox_caller_from_tool_context",
    "sandbox_id_or_missing_error_result",
    "sandbox_path_error_message",
    "sandbox_repo_root_from_tool_context",
    "resolve_tool_sandbox_path",
]
