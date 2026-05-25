"""Provider-neutral helpers for sandbox-backed tools."""

from __future__ import annotations

from collections.abc import Mapping
from typing import Any

from sandbox._shared.models import SandboxCaller
from tools._framework.core.context import ToolExecutionContextService
from tools._framework.core.results import ToolResult


def caller_from_context(
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
        task_center_run_id=str(context.get("task_center_run_id") or ""),
        task_center_task_id=str(context.get("task_center_task_id") or ""),
        task_center_attempt_id=str(context.get("task_center_attempt_id") or ""),
        task_center_goal_id=str(context.get("task_center_goal_id") or ""),
        task_center_request_id=str(context.get("task_center_request_id") or ""),
        tool_id=str(context.get("tool_id") or ""),
    )


def audit_kwargs_from_context(
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


def sandbox_audit_metadata(
    context: ToolExecutionContextService,
) -> dict[str, bool]:
    """Mark tool metadata when sandbox audit events were emitted directly."""
    return {"sandbox_audit_emitted": True} if audit_kwargs_from_context(context) else {}


def merge_tool_metadata(
    base: Mapping[str, Any] | None,
    extra: Mapping[str, Any],
) -> dict[str, Any]:
    merged = dict(base or {})
    merged.update(extra)
    return merged


def get_repo_root(context: ToolExecutionContextService) -> str:
    """Return the sandbox repository root for tool output/path resolution."""
    return str(context.get("repo_root") or "").strip()


def resolve_sandbox_path(path: str, context: ToolExecutionContextService) -> str:
    """Resolve a repo-relative path against the sandbox repository root."""
    # Trust boundary: absolute paths are passed through verbatim. The
    # sandbox provider (isolated rootfs) is the authoritative layer that
    # refuses or sandboxes host paths; this helper does not gate on them.
    if path.startswith("/"):
        return path
    repo_root = get_repo_root(context)
    if repo_root:
        return f"{repo_root.rstrip('/')}/{path}"
    return path


def normalized_path(path: str) -> str:
    """Return a stable absolute-or-relative path without trailing separators."""
    if path == "/":
        return path
    return path.rstrip("/") or path


def path_error(exc: Exception, path: str) -> str | None:
    """Return a user-facing path error when one can be recognized."""
    message = str(exc)
    if isinstance(exc, FileNotFoundError) or "No such file or directory" in message:
        return f"Path does not exist: {path}"
    return None


def sandbox_id_or_error(context: ToolExecutionContextService) -> tuple[str, ToolResult | None]:
    sandbox_id = str(context.get("sandbox_id") or "").strip()
    if sandbox_id:
        return sandbox_id, None
    return "", ToolResult(
        output="Sandbox id is unavailable.",
        is_error=True,
        metadata={"sandbox_required": True},
    )


__all__ = [
    "caller_from_context",
    "audit_kwargs_from_context",
    "get_repo_root",
    "merge_tool_metadata",
    "normalized_path",
    "path_error",
    "resolve_sandbox_path",
    "sandbox_audit_metadata",
    "sandbox_id_or_error",
]
