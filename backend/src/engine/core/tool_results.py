"""Post-dispatch result helpers for assistant tool calls."""

from __future__ import annotations

from message.messages import ToolResultBlock
from tools import ToolResult


def any_terminal_result(tool_results: list[ToolResultBlock]) -> bool:
    """True when a successful terminal tool result ended the query."""
    return any(result.does_terminate for result in tool_results)


def terminal_result_from_tool_results(
    tool_results: list[ToolResultBlock],
) -> ToolResult | None:
    for result in tool_results:
        if not result.does_terminate:
            continue
        return ToolResult(
            output=str(result.content),
            is_error=result.is_error,
            metadata=dict(result.metadata or {}),
            does_terminate=True,
        )
    return None
