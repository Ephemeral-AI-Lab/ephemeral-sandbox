"""Provider-neutral helpers for sandbox-backed tools."""

from __future__ import annotations

from sandbox.api import SandboxCaller
from tools.core.context import ToolExecutionContextService
from tools.core.results import ToolResult


def caller_from_context(
    context: ToolExecutionContextService,
    *,
    preferred_agent_id: str = "",
) -> SandboxCaller:
    """Build the sandbox caller identity for a tool call."""
    explicit = str(preferred_agent_id or "").strip()
    agent_run_id = str(context.agent_run_id or "")
    agent_id = explicit or agent_run_id.strip() or str(context.agent_name or "").strip()
    return SandboxCaller(
        agent_id=agent_id,
        run_id=str(context.get("run_id") or ""),
        agent_run_id=agent_run_id,
        task_id=str(context.get("task_id") or ""),
    )


def get_repo_root(context: ToolExecutionContextService) -> str:
    """Return the sandbox repository root for tool output/path resolution."""
    return str(context.get("repo_root") or "").strip()


def resolve_sandbox_path(path: str, context: ToolExecutionContextService) -> str:
    """Resolve a repo-relative path against the sandbox repository root."""
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
    "get_repo_root",
    "normalized_path",
    "path_error",
    "resolve_sandbox_path",
    "sandbox_id_or_error",
]
