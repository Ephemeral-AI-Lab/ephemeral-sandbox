"""Daytona workspace discovery and runtime context metadata.

The ``sandbox`` argument uses Daytona SDK shape: ``project_dir``, ``labels``,
and ``process.exec``. Keep this provider-owned until the lifecycle layer no
longer needs raw provider objects for workspace discovery.
"""

from __future__ import annotations

from typing import Any


def _sandbox_project_root(sandbox: Any) -> str | None:
    project_dir = getattr(sandbox, "project_dir", None)
    if isinstance(project_dir, str) and project_dir.strip():
        return project_dir.strip()
    labels = getattr(sandbox, "labels", None)
    if isinstance(labels, dict):
        label_dir = labels.get("project_dir")
        if isinstance(label_dir, str) and label_dir.strip():
            return label_dir.strip()
    return None


def _workspace_from_pwd_response(resp: Any) -> str | None:
    if getattr(resp, "exit_code", None) != 0 or not getattr(resp, "result", None):
        return None
    result = str(resp.result).strip()
    return result or None


def discover_workspace(sandbox: Any) -> str | None:
    project_dir = _sandbox_project_root(sandbox)
    if project_dir:
        return project_dir
    try:
        return _workspace_from_pwd_response(sandbox.process.exec("pwd"))
    except Exception:
        pass
    return None


async def discover_workspace_async(sandbox: Any) -> str | None:
    project_dir = _sandbox_project_root(sandbox)
    if project_dir:
        return project_dir
    try:
        return _workspace_from_pwd_response(await sandbox.process.exec("pwd"))
    except Exception:
        pass
    return None


def prepare_sandbox_runtime_context(
    context: Any,
    *,
    sandbox: Any,
    workspace_root: str | None,
) -> None:
    """Inject shared sandbox runtime metadata.

    Provider implementations own provider-specific context keys and adapter
    registration. This helper only normalizes workspace metadata shared by
    sandbox tools.
    """
    repo_root = str(context.get("repo_root") or "").strip()
    if not repo_root:
        candidate = str(workspace_root or "").strip()
        if not candidate and sandbox is not None:
            candidate = _sandbox_project_root(sandbox) or ""
        if candidate:
            repo_root = candidate
            context["repo_root"] = repo_root

    if not context.get("exec_cwd") and repo_root:
        context["exec_cwd"] = repo_root


__all__ = [
    "discover_workspace",
    "discover_workspace_async",
    "prepare_sandbox_runtime_context",
]
