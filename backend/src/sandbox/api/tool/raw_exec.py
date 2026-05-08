"""Un-guarded sandbox command execution.

This primitive is reserved for runtime setup, status/control, and debug paths.
Agent-visible shell execution must go through the guarded public verbs.
"""

from __future__ import annotations

from sandbox.contract import RawExecResult
from sandbox.provider.registry import get_adapter


async def raw_exec(
    sandbox_id: str,
    command: str,
    *,
    cwd: str | None = None,
    timeout: int | None = None,
) -> RawExecResult:
    """Run *command* through the registered provider adapter."""
    return await get_adapter(sandbox_id).exec(
        sandbox_id,
        command,
        cwd=cwd,
        timeout=timeout,
    )


__all__ = ["raw_exec"]
