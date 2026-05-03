"""Daytona implementation of the provider adapter seam."""

from __future__ import annotations

import re
import shlex
from collections.abc import Awaitable, Callable
from typing import Any, ClassVar

from sandbox.api.models import RawExecResult
from sandbox.client.async_ import get_async_sandbox

_EXIT_MARKER = "__CODEX_EXIT_CODE__="
_USER_LOCAL_BIN_EXPORT = 'export PATH="$HOME/.local/bin:$PATH"'
_PROJECT_VENV_BIN_EXPORT = 'if [ -d .venv/bin ]; then export PATH="$PWD/.venv/bin:$PATH"; fi'
_PYTHON3_SHIM = (
    'if ! command -v python >/dev/null 2>&1 && command -v python3 >/dev/null 2>&1; '
    'then python() { command python3 "$@"; }; fi'
)
_TRAILING_TERM_NOISE_RE = re.compile(
    r"(?:\x1b\[[0-9;]*[A-Za-z]|TERM environment variable not set\.)+\s*$"
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


def _wrap_bash_command(command: str, *, cwd: str | None = None) -> str:
    cd_command = f"cd {shlex.quote(cwd)}\n" if cwd else ""
    script = (
        f"{_USER_LOCAL_BIN_EXPORT}\n"
        f"{cd_command}"
        f"{_PROJECT_VENV_BIN_EXPORT}\n"
        f"{_PYTHON3_SHIM}\n"
        f"{command}\n"
        "__codex_exit_code=$?\n"
        f'printf "\\n{_EXIT_MARKER}%s\\n" "$__codex_exit_code"\n'
        'exit "$__codex_exit_code"'
    )
    return f"env -u LC_ALL bash -o pipefail -lc {shlex.quote(script)}"


def _extract_exit_code(
    output: str,
    *,
    fallback_exit_code: int | str | None,
) -> tuple[str, int]:
    sanitized = _TRAILING_TERM_NOISE_RE.sub("", output or "").rstrip()
    matches = list(re.finditer(rf"\n?{re.escape(_EXIT_MARKER)}(-?\d+)", sanitized, flags=re.S))
    if matches:
        marker = matches[-1]
        resolved = int(marker.group(1))
        cleaned = sanitized[: marker.start()]
        if cleaned.endswith("\n"):
            cleaned = cleaned[:-1]
        return cleaned, resolved
    if fallback_exit_code is None:
        return sanitized, 0
    if isinstance(fallback_exit_code, int):
        return sanitized, fallback_exit_code
    stripped = fallback_exit_code.strip()
    if stripped.lstrip("-").isdigit():
        return sanitized, int(stripped)
    return sanitized, 0


__all__ = ["DaytonaProviderAdapter", "_EXIT_MARKER", "_extract_exit_code", "_wrap_bash_command"]
