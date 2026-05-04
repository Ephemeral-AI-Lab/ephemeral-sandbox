"""Daytona implementation of the provider adapter seam."""

from __future__ import annotations

from collections.abc import Awaitable, Callable
from typing import Any, ClassVar

from sandbox.api import RawExecResult
from sandbox.providers.daytona.client.async_ import get_async_sandbox
from sandbox.bash import (
    EXIT_MARKER as _EXIT_MARKER,
    extract_exit_code as _extract_exit_code,
    wrap_bash_command as _wrap_bash_command,
)


class DaytonaProviderAdapter:
    """Provider adapter backed directly by the AsyncDaytona SDK."""

    name: ClassVar[str] = "daytona"

    def __init__(
        self,
        *,
        sandbox_resolver: Callable[[str], Awaitable[Any]] | None = None,
    ) -> None:
        self._resolver = sandbox_resolver or get_async_sandbox

    async def exec(
        self,
        sandbox_id: str,
        command: str,
        *,
        cwd: str | None = None,
        timeout: int | None = None,
    ) -> "RawExecResult":
        sandbox = await self._resolve(sandbox_id)
        wrapped = _wrap_bash_command(command, cwd=cwd)
        kwargs: dict[str, Any] = {}
        if timeout is not None:
            kwargs["timeout"] = timeout
        response = await sandbox.process.exec(wrapped, **kwargs)
        stdout, exit_code = _extract_exit_code(
            getattr(response, "result", "") or "",
            fallback_exit_code=getattr(response, "exit_code", None),
        )
        return RawExecResult(
            success=exit_code == 0,
            exit_code=exit_code,
            stdout=stdout,
            stderr=str(getattr(response, "stderr", "") or ""),
        )

    async def _resolve(self, sandbox_id: str) -> Any:
        return await self._resolver(sandbox_id)


__all__ = ["DaytonaProviderAdapter", "_EXIT_MARKER", "_extract_exit_code", "_wrap_bash_command"]
