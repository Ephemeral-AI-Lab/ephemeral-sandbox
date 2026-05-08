"""``api.shell`` dispatch entry."""

from __future__ import annotations

from sandbox.runtime.daemon.service import shell_runner


async def shell(args: dict[str, object]) -> dict[str, object]:
    """Run a guarded shell command through the command-exec service."""
    return await shell_runner.execute_shell_api(args)


__all__ = ["shell"]
