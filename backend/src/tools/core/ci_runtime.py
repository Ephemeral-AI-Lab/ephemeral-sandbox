"""Code-intelligence service accessor and standard error builders.

Mutation-capable tools reach the code-intelligence service through
:func:`get_ci_service` and build consistent fail-closed responses via
:func:`ci_required_result` and :func:`ci_write_required_result` when the
service is unavailable. Actor attribution lives in
:mod:`tools.core.ci_attribution`.
"""

from __future__ import annotations

from typing import Any

from tools.core.base import ToolExecutionContextService, ToolResult

__all__ = [
    "ci_required_result",
    "ci_write_required_result",
    "get_ci_service",
]


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
