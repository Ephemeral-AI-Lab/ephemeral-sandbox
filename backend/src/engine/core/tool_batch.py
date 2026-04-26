"""Tool-batch validation helpers for the query loop."""

from __future__ import annotations

from typing import TYPE_CHECKING, Any

from message.messages import ToolResultBlock

if TYPE_CHECKING:
    from engine.core.query import QueryContext


def reject_tool_batch(
    tool_calls: list[Any],
    message: str,
) -> list[ToolResultBlock]:
    return [
        ToolResultBlock(tool_use_id=str(tc.id), content=message, is_error=True)
        for tc in tool_calls
    ]


def validate_tool_batch(
    context: QueryContext,
    tool_calls: list[Any],
) -> list[ToolResultBlock] | None:
    if not tool_calls or len(tool_calls) <= 1:
        return None

    # Terminal-tool exclusivity: a terminal tool ends the run, so it cannot
    # execute coherently beside sibling tool calls.
    terminal_in_batch = [
        tc for tc in tool_calls if tc.name in context.terminal_tools
    ]

    if not terminal_in_batch:
        return None

    flagged_names = ", ".join(sorted({f"`{tc.name}`" for tc in terminal_in_batch}))
    called_names = ", ".join(f"`{tc.name}`" for tc in tool_calls)
    message = (
        f"Terminal tool {flagged_names} must be called alone. "
        f"This response batched it with other tools: {called_names}. "
        f"No tool in this batch executed. "
        f"Resubmit with only the exclusive tool in its own final batch."
    )
    return reject_tool_batch(tool_calls, message=message)
