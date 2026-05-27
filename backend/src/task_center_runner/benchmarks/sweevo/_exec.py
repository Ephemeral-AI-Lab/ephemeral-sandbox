"""Sandbox command-execution helpers for SWE-EVO."""

from __future__ import annotations

import asyncio
import logging

import sandbox.api as sandbox_api

from task_center_runner.benchmarks.sweevo.models import (
    _DEFAULT_SANDBOX_COMMAND_TIMEOUT,
)

logger = logging.getLogger(__name__)


def _is_transient_sandbox_exec_error(exc: Exception) -> bool:
    text = str(exc).lower()
    return (
        "connection reset" in text
        or "connection refused" in text
        or "server disconnected" in text
        or "failed to execute command" in text
        or "clientoserror" in text
        or "temporarily unavailable" in text
    )


async def _wait_for_sandbox_exec_ready(
    sandbox_id: str,
    *,
    attempts: int = 6,
    delay_s: float = 1.0,
) -> None:
    """Wait until a started sandbox accepts toolbox exec requests."""
    last_exc: Exception | None = None
    for attempt in range(1, attempts + 1):
        try:
            await _exec(sandbox_id, "pwd", cwd="/", timeout=10)
            return
        except Exception as exc:
            last_exc = exc
            if not _is_transient_sandbox_exec_error(exc):
                raise

        if attempt < attempts:
            logger.warning(
                "SWE-EVO sandbox %s exec readiness probe failed (attempt %s/%s): %s",
                sandbox_id,
                attempt,
                attempts,
                last_exc,
            )
            await asyncio.sleep(delay_s)

    assert last_exc is not None
    raise RuntimeError(f"SWE-EVO sandbox {sandbox_id} did not become exec-ready") from last_exc


async def _exec(
    sandbox_id: str,
    cmd: str,
    timeout: int = _DEFAULT_SANDBOX_COMMAND_TIMEOUT,
    cwd: str | None = None,
    *,
    check: bool = True,
) -> str:
    """Execute *cmd* in the sandbox via the provider raw-exec primitive."""
    try:
        response = await sandbox_api.raw_exec(
            sandbox_id,
            cmd,
            cwd=cwd or "/",
            timeout=timeout,
        )
        stdout = getattr(response, "stdout", "") or ""
        stderr = getattr(response, "stderr", "") or ""
        result_text = stdout if not stderr else f"{stdout}\n{stderr}" if stdout else stderr
        exit_code = response.exit_code
        if exit_code not in (None, 0):
            message = (
                f"Sandbox command failed with exit code {exit_code}: {cmd[:100]}\n"
                f"Output: {result_text[:500]}"
            )
            logger.warning(message)
            if check:
                raise RuntimeError(message)
        return result_text
    except Exception as exc:
        if check and isinstance(exc, RuntimeError):
            raise
        logger.warning("Sandbox exec failed: %s\nCommand: %s", exc, cmd[:100])
        if check:
            raise
        return f"ERROR: {exc}"


__all__ = ["_exec", "_wait_for_sandbox_exec_ready", "_is_transient_sandbox_exec_error"]
