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
        ToolResultBlock(tool_use_id=str(tc.id), content=message, is_error=True) for tc in tool_calls
    ]


def validate_tool_batch(
    context: QueryContext,
    tool_calls: list[Any],
) -> list[ToolResultBlock] | None:
    if not tool_calls:
        return None

    # Terminal-tool exclusivity: if any tool in this batch is a declared
    # terminal tool, it must be the ONLY tool. Mixing a terminal tool with
    # siblings would let siblings mutate state after the agent has already
    # submitted its terminal result.
    if context.terminal_tools and len(tool_calls) > 1:
        terminal_in_batch = [tc for tc in tool_calls if tc.name in context.terminal_tools]
        if terminal_in_batch:
            terminal_names = ", ".join(f"`{tc.name}`" for tc in terminal_in_batch)
            called = ", ".join(f"`{tc.name}`" for tc in tool_calls)
            message = (
                f"Terminal tool {terminal_names} must be called alone. "
                f"This response batched it with other tools: {called}. "
                f"No tool in this batch executed. "
                f"Resubmit with only the terminal tool in its own final batch."
            )
            return reject_tool_batch(tool_calls, message=message)

    return None
