"""Sandbox workspace discovery and runtime context metadata."""

from __future__ import annotations

import logging
import os
from typing import Any

logger = logging.getLogger(__name__)

SANDBOX_RUNTIME_BOOTSTRAP_ENV = "EOS_SANDBOX_RUNTIME_BOOTSTRAP"


def _sandbox_runtime_bootstrap_enabled() -> bool:
    """Return True when eager sandbox-runtime bootstrap is enabled."""
    return os.environ.get(SANDBOX_RUNTIME_BOOTSTRAP_ENV) == "1"


async def bootstrap_in_sandbox_runtime(
    sandbox_id: str,
    workspace_root: str,
) -> None:
    """Upload the runtime command bundle during sandbox lifecycle events.

    Called by ``SandboxService.create_sandbox`` and ``start_sandbox`` after
    the underlying Daytona sandbox is provisioned/resumed.

    Short-circuits as a no-op when eager bootstrap is disabled, or when
    ``sandbox_id`` or ``workspace_root`` is empty. Raises when the runtime
    bundle cannot be prepared.
    """
    if not _sandbox_runtime_bootstrap_enabled():
        return
    if not sandbox_id or not str(workspace_root or "").strip():
        return

    from sandbox.runtime.bundle import ensure_runtime_uploaded

    logger.info(
        "eager sandbox-runtime bootstrap starting for sandbox %s at %s",
        sandbox_id,
        workspace_root,
    )
    await ensure_runtime_uploaded(sandbox_id)
    logger.info(
        "eager sandbox-runtime bootstrap completed for sandbox %s at %s",
        sandbox_id,
        workspace_root,
    )


async def bootstrap_upload_runtime_bundle(
    sandbox_id: str,
    workspace_root: str,
) -> None:
    """Upload-only phase of the eager bootstrap.

    Performs the chunked bundle upload without spawning the daemon. The
    create-sandbox path runs this concurrently with ``ensure_git`` (which
    is the other long pre-bootstrap step), then defers to the regular
    :func:`bootstrap_in_sandbox_runtime` afterwards. That call finds
    the bundle already in place via ``.bundle-hash``. Net effect: the
    upload's wall time overlaps with ``ensure_git``
    instead of stacking on top of it.

    Same gating as :func:`bootstrap_in_sandbox_runtime`. Raises on upload
    failure; callers running this in a background thread are expected to
    swallow and let the sequential bootstrap retry.
    """
    if not _sandbox_runtime_bootstrap_enabled():
        return
    if not sandbox_id or not str(workspace_root or "").strip():
        return

    from sandbox.runtime.bundle import ensure_runtime_uploaded

    logger.info(
        "eager sandbox-runtime bundle upload starting for sandbox %s",
        sandbox_id,
    )
    await ensure_runtime_uploaded(sandbox_id)
    logger.info(
        "eager sandbox-runtime bundle upload completed for sandbox %s",
        sandbox_id,
    )


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


def discover_workspace(sandbox: Any) -> str | None:
    project_dir = _sandbox_project_root(sandbox)
    if project_dir:
        return project_dir
    try:
        resp = sandbox.process.exec("pwd")
        if resp.exit_code == 0 and resp.result:
            return resp.result.strip()
    except Exception:
        pass
    return None


async def discover_workspace_async(sandbox: Any) -> str | None:
    project_dir = _sandbox_project_root(sandbox)
    if project_dir:
        return project_dir
    try:
        resp = await sandbox.process.exec("pwd")
        if resp.exit_code == 0 and resp.result:
            return resp.result.strip()
    except Exception:
        pass
    return None


def prepare_sandbox_runtime_context(
    context: Any,
    *,
    sandbox: Any,
    workspace_root: str | None,
) -> None:
    """Inject provider-neutral sandbox runtime metadata.

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
