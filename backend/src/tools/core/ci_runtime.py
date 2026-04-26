"""Code-intelligence service accessor and standard error builders.

Mutation-capable tools reach the code-intelligence service through
:func:`get_ci_service` and build consistent fail-closed responses via
:func:`ci_required_result` and :func:`ci_write_required_result` when the
service is unavailable. Actor attribution lives in
:mod:`tools.core.ci_attribution`.
"""

from __future__ import annotations

import logging
from typing import Any

from tools.core.base import ToolExecutionContextService, ToolResult

logger = logging.getLogger(__name__)

__all__ = [
    "ci_required_result",
    "ci_write_required_result",
    "get_ci_service",
    "resolve_sandbox",
]


async def resolve_sandbox(context: ToolExecutionContextService) -> Any | None:
    """Return the bound Daytona sandbox, lazily attaching from ``sandbox_id``."""
    from tools.core.sandbox_runtime import get_daytona_sandbox

    sandbox = get_daytona_sandbox(context)
    if sandbox is not None:
        return sandbox
    sandbox_id = str(context.get("sandbox_id") or "").strip()
    if not sandbox_id:
        return None
    try:
        from sandbox.async_client import get_async_sandbox

        sandbox = await get_async_sandbox(sandbox_id)
        context["daytona_sandbox"] = sandbox
        return sandbox
    except Exception:
        logger.debug("Lazy sandbox attach failed for %s", sandbox_id, exc_info=True)
        return None


def get_ci_service(context: ToolExecutionContextService) -> Any | None:
    """Return the :class:`CodeIntelligenceService` bound to *context*, if any."""
    return context.ci_service


def ci_required_result(tool_name: str, detail: str) -> ToolResult:
    """Error payload for tools that refuse to run without code intelligence."""
    suffix = str(detail or "").strip()
    return ToolResult(
        output=(
            f"{tool_name}: Code intelligence service is unavailable."
            f"{' ' + suffix if suffix else ''}"
        ),
        is_error=True,
        metadata={"ci_required": True},
    )


def ci_write_required_result(
    tool_name: str,
    file_path: str,
    *,
    conflict: bool = False,
) -> ToolResult:
    """Error payload for mutation tools that refuse to fall back to raw shell I/O."""
    metadata: dict[str, Any] = {"ci_required": True}
    if conflict:
        metadata["conflict"] = True
    operation = "Write" if "write" in tool_name else "Edit"
    return ToolResult(
        output=(
            f"{tool_name}: Code intelligence service is unavailable. "
            f"{operation} of {file_path} is disabled. Direct sandbox write fallback is disabled."
        ),
        is_error=True,
        metadata=metadata,
    )
