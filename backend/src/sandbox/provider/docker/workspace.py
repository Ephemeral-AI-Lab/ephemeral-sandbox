"""Docker workspace discovery and runtime context metadata.

The ``container`` argument here is a ``docker.models.containers.Container``
object from the Docker Python SDK (or, in tests, a duck-typed equivalent with
``.attrs`` dict and ``.exec_run`` method). Public surface matches
``provider/daytona/workspace.py`` so call sites in ``context_preparer.py`` are
symmetric.
"""

from __future__ import annotations

from typing import Any


def _container_project_root(container: Any) -> str | None:
    """Resolve project root from container labels / Config.WorkingDir."""
    attrs = getattr(container, "attrs", None) or {}
    config = attrs.get("Config") or {}
    labels = config.get("Labels") or {}
    if isinstance(labels, dict):
        project_dir = labels.get("project_dir")
        if isinstance(project_dir, str) and project_dir.strip():
            return project_dir.strip()
    working_dir = config.get("WorkingDir")
    if isinstance(working_dir, str) and working_dir.strip():
        return working_dir.strip()
    return None


def _decode_exec_output(output: Any) -> str:
    if isinstance(output, bytes):
        return output.decode("utf-8", errors="replace").strip()
    if isinstance(output, tuple) and output:
        return _decode_exec_output(output[-1])
    if output is None:
        return ""
    return str(output).strip()


def _workspace_from_pwd_exec(container: Any) -> str | None:
    try:
        result = container.exec_run("pwd")
    except Exception:
        return None
    exit_code = getattr(result, "exit_code", None)
    if exit_code is None and isinstance(result, tuple) and len(result) == 2:
        exit_code, output = result
    else:
        output = getattr(result, "output", None)
    if exit_code != 0:
        return None
    text = _decode_exec_output(output)
    return text or None


def discover_workspace(container: Any) -> str | None:
    project_dir = _container_project_root(container)
    if project_dir:
        return project_dir
    return _workspace_from_pwd_exec(container)


async def discover_workspace_async(container: Any) -> str | None:
    project_dir = _container_project_root(container)
    if project_dir:
        return project_dir
    import asyncio

    return await asyncio.to_thread(_workspace_from_pwd_exec, container)


def prepare_sandbox_runtime_context(
    context: Any,
    *,
    container: Any,
    workspace_root: str | None,
) -> None:
    """Inject shared sandbox runtime metadata.

    Symmetric with :func:`sandbox.provider.daytona.workspace.prepare_sandbox_runtime_context`:
    only normalizes ``repo_root`` and ``exec_cwd`` keys. Adapter registration
    happens in :mod:`sandbox.provider.docker.context_preparer`.
    """
    repo_root = str(context.get("repo_root") or "").strip()
    if not repo_root:
        candidate = str(workspace_root or "").strip()
        if not candidate and container is not None:
            candidate = _container_project_root(container) or ""
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
