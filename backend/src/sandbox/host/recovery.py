"""Provider-neutral sandbox recovery (probe + restart).

Rewritten against the registered provider adapter primitives; no concrete
provider imports live here.

Long-running benchmark runs sometimes observe a sandbox that still resolves
by id yet whose backing container is gone or detached. Probe via
provider ``exec`` first; if the probe fails, restart through the provider adapter
once and re-run the post-start setup hook.
"""

from __future__ import annotations

import logging
from typing import Any

from sandbox.host.setup import setup_after_start
from sandbox.provider.registry import get_adapter
from sandbox.async_bridge import run_sync

logger = logging.getLogger(__name__)


def ensure_running(sandbox_id: str) -> dict[str, Any]:
    """Best-effort recovery: probe, restart on failure, re-run setup hook."""
    adapter = get_adapter(sandbox_id)
    info = adapter.get(sandbox_id)
    try:
        resp = run_sync(adapter.exec(sandbox_id, "pwd", timeout=10))
        exit_code = getattr(resp, "exit_code", 0)
        if exit_code in (None, 0):
            return info
    except Exception:
        logger.warning(
            "Sandbox %s probe failed; attempting restart recovery",
            sandbox_id,
            exc_info=True,
        )

    try:
        adapter.start(sandbox_id)
    except Exception:
        logger.debug(
            "Sandbox %s start during recovery raised; refreshing handle",
            sandbox_id,
            exc_info=True,
        )

    # WR-02: unconditionally refresh; previous code had a dead-write
    # pair that ran adapter.get up to three times before this line
    # silently shadowed all of them.
    info = adapter.get(sandbox_id)

    workspace_root = info.get("project_dir") or ""
    setup_after_start(sandbox_id, workspace_root)

    return info


__all__ = ["ensure_running"]
