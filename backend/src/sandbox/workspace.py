"""Sandbox workspace discovery — resolve cwd and inject code intelligence services."""

from __future__ import annotations

import logging
from typing import Any

logger = logging.getLogger(__name__)


def discover_workspace(sandbox: Any) -> str | None:
    project_dir = getattr(sandbox, "project_dir", None)
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
    project_dir = getattr(sandbox, "project_dir", None)
    if project_dir:
        return project_dir
    try:
        resp = await sandbox.process.exec("pwd")
        if resp.exit_code == 0 and resp.result:
            return resp.result.strip()
    except Exception:
        pass
    return None


def inject_code_intelligence(
    context: Any,
    sandbox_id: str | None,
    sandbox: Any,
    workspace_root: str,
) -> None:
    if sandbox_id and "ci_service" not in context.metadata:
        try:
            from code_intelligence.routing.service import get_code_intelligence

            svc = get_code_intelligence(
                sandbox_id=sandbox_id,
                workspace_root=workspace_root,
                sandbox=sandbox,
            )
            context.metadata["ci_service"] = svc
        except Exception:
            logger.debug("CI service not available for sandbox %s", sandbox_id)
