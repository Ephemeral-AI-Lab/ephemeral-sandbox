"""Runtime handler for raw overlay capture requests."""

from __future__ import annotations

from typing import Any

from sandbox.overlay.engine import LocalOverlayEngine
from sandbox.overlay.wire import overlay_outcome_to_dict


async def handle(args: dict[str, Any]) -> dict[str, Any]:
    engine = LocalOverlayEngine(
        sandbox_id=str(args.get("sandbox_id") or "local"),
        workspace_root=str(args.get("workspace_root") or "/workspace"),
        direct_runtime=True,
    )
    timeout_raw = args.get("timeout")
    timeout = int(timeout_raw) if timeout_raw is not None else None
    outcome = await engine.execute(
        str(args["command"]),
        timeout=timeout,
        stdin=args.get("stdin"),
        description=str(args.get("description") or ""),
        agent_id=str(args.get("agent_id") or ""),
    )
    return overlay_outcome_to_dict(outcome)


__all__ = ["handle"]
