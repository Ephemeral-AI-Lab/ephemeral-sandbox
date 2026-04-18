"""Tool execution logic — handles a single tool call end-to-end."""

from __future__ import annotations

from typing import TYPE_CHECKING, Any

from hooks import HookEvent
from message.messages import ToolResultBlock
from tools.core.base import ExecutionMetadata, ToolExecutionContext, run_tool_safely
from tools.core.runtime import merge_runtime_metadata

if TYPE_CHECKING:
    from engine.core.query import QueryContext


def _build_budget_exceeded_error(
    tool_use_id: str,
    tool_call_limit: int,
) -> ToolResultBlock:
    return ToolResultBlock(
        tool_use_id=tool_use_id,
        content=(
            f"tool_call_limit exceeded: {tool_call_limit} tool "
            f"calls already used. The agent run will terminate after "
            f"this turn — wrap up and summarize your progress now to "
            f"preserve partial work."
        ),
        is_error=True,
    )


def _consume_tool_budget_or_reject(
    context: QueryContext,
    tool_name: str,
    tool_use_id: str,
) -> ToolResultBlock | None:
    if context.tool_call_limit is None:
        return None
    if context.tool_calls_used >= context.tool_call_limit:
        if tool_name in context.terminal_tools:
            return None
        return _build_budget_exceeded_error(tool_use_id, context.tool_call_limit)
    context.tool_calls_used += 1
    return None


async def execute_tool_call(
    context: QueryContext,
    tool_name: str,
    tool_use_id: str,
    tool_input: dict[str, object],
    extra_metadata: ExecutionMetadata | dict[str, Any] | None = None,
) -> ToolResultBlock:
    budget_rejection = _consume_tool_budget_or_reject(context, tool_name, tool_use_id)
    if budget_rejection is not None:
        return budget_rejection

    if context.hook_executor is not None:
        pre_hooks = await context.hook_executor.execute(
            HookEvent.PRE_TOOL_USE,
            {
                "tool_name": tool_name,
                "tool_input": tool_input,
                "event": HookEvent.PRE_TOOL_USE.value,
            },
        )
        if pre_hooks.blocked:
            return ToolResultBlock(
                tool_use_id=tool_use_id,
                content=pre_hooks.reason or f"pre_tool_use hook blocked {tool_name}",
                is_error=True,
            )

    tool = context.tool_registry.get(tool_name)
    if tool is None:
        return ToolResultBlock(
            tool_use_id=tool_use_id,
            content=f"Unknown tool: {tool_name}",
            is_error=True,
        )

    metadata = (
        context.tool_metadata.copy() if context.tool_metadata is not None else ExecutionMetadata()
    )
    metadata.tool_registry = context.tool_registry
    metadata.tool_id = tool_use_id
    if extra_metadata:
        metadata.update(extra_metadata)

    result = await run_tool_safely(
        tool,
        tool_input,
        ToolExecutionContext(cwd=context.cwd, metadata=metadata),
    )
    merge_runtime_metadata(
        original=context.tool_metadata, updated=metadata, result_metadata=result.metadata
    )
    if not result.is_error:
        from engine.runtime.tool_trace import record_tool_trace

        record_tool_trace(
            context.tool_metadata,
            tool_name,
            tool_input,
            tool_use_id=tool_use_id,
        )

    tool_result = ToolResultBlock(
        tool_use_id=tool_use_id,
        content=result.output,
        is_error=result.is_error,
        metadata=result.metadata,
    )
    if context.hook_executor is not None:
        await context.hook_executor.execute(
            HookEvent.POST_TOOL_USE,
            {
                "tool_name": tool_name,
                "tool_input": tool_input,
                "tool_output": tool_result.content,
                "tool_is_error": tool_result.is_error,
                "event": HookEvent.POST_TOOL_USE.value,
            },
        )
    return tool_result
