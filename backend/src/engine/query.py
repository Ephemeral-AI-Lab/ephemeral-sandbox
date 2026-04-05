"""Core tool-aware query loop."""

from __future__ import annotations

import asyncio
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING, AsyncIterator

if TYPE_CHECKING:
    from utils.compact import SessionState

from models.types import (
    ApiCancelEvent,
    ApiMessageCompleteEvent,
    ApiMessageRequest,
    ApiTextDeltaEvent,
    ApiThinkingDeltaEvent,
    ApiToolUseDeltaEvent,
    SupportsStreamingMessages,
    UsageSnapshot,
)
from engine.messages import ConversationMessage, TextBlock, ToolResultBlock, ToolUseBlock
from engine.stream_events import (
    AssistantTextDelta,
    AssistantTurnComplete,
    StreamEvent,
    ThinkingDelta,
    ToolExecutionCancelled,
    ToolExecutionCompleted,
    ToolExecutionStarted,
)
from engine.streaming_executor import StreamingToolExecutor
from hooks import HookEvent, HookExecutor
from tools.base import ToolExecutionContext, ToolRegistry


@dataclass
class QueryContext:
    """Context shared across a query run."""

    api_client: SupportsStreamingMessages
    tool_registry: ToolRegistry
    cwd: Path
    model: str
    system_prompt: str
    max_tokens: int
    max_turns: int = 200
    hook_executor: HookExecutor | None = None
    tool_metadata: dict[str, object] | None = None
    session_state: "SessionState | None" = None


async def _run_query_loop(
    context: QueryContext,
    messages: list[ConversationMessage],
) -> AsyncIterator[tuple[StreamEvent, UsageSnapshot | None]]:
    """Inner loop — yields events. Runs until model stops requesting tools.

    Features:
    - Mid-stream tool detection: tools start executing as tool_use blocks arrive
    - Progress streaming: LLM sees tool output as it happens
    - LLM abort: LLM can cancel running tools via [CANCEL:tool_id reason="..."]
    """
    import re
    from utils.compact import (
        SessionState,
        auto_compact_if_needed,
    )

    compact_state = context.session_state or SessionState()
    cancel_pattern = re.compile(r'\[CANCEL:(\S+)(?:\s+reason="([^"]*)")?\]')

    for _ in range(context.max_turns):
        messages, was_compacted = await auto_compact_if_needed(
            messages,
            api_client=context.api_client,
            model=context.model,
            system_prompt=context.system_prompt,
            state=compact_state,
        )

        executor = StreamingToolExecutor(
            tool_registry=context.tool_registry,
            context=ToolExecutionContext(
                cwd=context.cwd,
                metadata=context.tool_metadata or {},
            ),
        )

        # Inject Daytona sandbox into context if DaytonaToolkit is registered
        daytona_toolkit = context.tool_registry.get_toolkit("sandbox_operations")
        if daytona_toolkit is not None:
            await daytona_toolkit.prepare_context_async(executor._context)

        final_message: ConversationMessage | None = None
        usage = UsageSnapshot()
        pending_cancel: dict[str, str] = {}

        async for event in context.api_client.stream_message(
            ApiMessageRequest(
                model=context.model,
                messages=messages,
                system_prompt=context.system_prompt,
                max_tokens=context.max_tokens,
                tools=context.tool_registry.to_api_schema(),
            )
        ):
            if isinstance(event, ApiThinkingDeltaEvent):
                yield ThinkingDelta(text=event.text), None
                continue

            if isinstance(event, ApiTextDeltaEvent):
                if match := cancel_pattern.search(event.text):
                    tool_id, reason = match.groups()
                    pending_cancel[tool_id] = reason or "Cancelled by LLM"
                yield AssistantTextDelta(text=event.text), None
                continue

            if isinstance(event, ApiToolUseDeltaEvent):
                assistant_msg = final_message or ConversationMessage(role="assistant", content=[])
                started = executor.add_tool(event, assistant_msg)
                if started:
                    yield started, None
                for progress in executor.get_progress():
                    yield progress, None
                continue

            if isinstance(event, ApiCancelEvent):
                executor.cancel(event.tool_id, event.reason)
                continue

            if isinstance(event, ApiMessageCompleteEvent):
                final_message = event.message
                usage = event.usage

        if final_message is None:
            raise RuntimeError(
                f"Model stream finished without a final message for model {context.model}. "
                "Check that the API endpoint, authentication, and model name are correct."
            )

        # Process pending cancels from text
        for tool_id, reason in pending_cancel.items():
            executor.cancel(tool_id, reason)

        # Yield any remaining progress
        for progress in executor.get_progress():
            yield progress, None

        messages.append(final_message)
        yield AssistantTurnComplete(message=final_message, usage=usage), usage

        if not final_message.tool_uses:
            return

        # Yield started events for any remaining tools
        for started in executor.get_started_events():
            yield started, None

        # Wait for all tools to complete and yield results
        tool_results: list[ToolResultBlock] = []
        for completed in executor.get_remaining():
            if isinstance(completed, ToolExecutionCompleted):
                tool_results.append(
                    ToolResultBlock(
                        tool_use_id="",  # filled by caller
                        content=completed.output,
                        is_error=completed.is_error,
                    )
                )
                yield completed, None
            elif isinstance(completed, ToolExecutionCancelled):
                tool_results.append(
                    ToolResultBlock(
                        tool_use_id="",
                        content=f"[CANCELLED] {completed.reason}",
                        is_error=True,
                    )
                )
                yield completed, None

        # Match tool results to their tool_use blocks
        if not tool_results:
            tool_calls = final_message.tool_uses
            if len(tool_calls) == 1:
                tc = tool_calls[0]
                result = await _execute_tool_call(context, tc.name, tc.id, tc.input)
                tool_results = [result]
                yield (
                    ToolExecutionCompleted(
                        tool_name=tc.name,
                        output=result.content,
                        is_error=result.is_error,
                    ),
                    None,
                )
            else:
                started_events = []
                for tc in tool_calls:
                    started_events.append(
                        ToolExecutionStarted(
                            tool_name=tc.name,
                            tool_input=tc.input,
                        )
                    )
                    yield started_events[-1], None

                results = await asyncio.gather(
                    *[_execute_tool_call(context, tc.name, tc.id, tc.input) for tc in tool_calls]
                )
                tool_results = list(results)
                for tc, result in zip(tool_calls, results):
                    yield (
                        ToolExecutionCompleted(
                            tool_name=tc.name,
                            output=result.content,
                            is_error=result.is_error,
                        ),
                        None,
                    )

        # Fill in tool_use_id for results that need it
        tool_use_map = {tu.id: tu for tu in final_message.tool_uses}
        for tr in tool_results:
            for tu_id, tu in tool_use_map.items():
                if tr.tool_use_id == "" or tr.tool_use_id == tu_id:
                    tr.tool_use_id = tu_id
                    break

        messages.append(ConversationMessage(role="user", content=tool_results))  # type: ignore[arg-type]

    yield (
        ToolExecutionCompleted(
            tool_name="",
            output=f"Agent stopped: maximum turn limit ({context.max_turns}) reached. "
            "The conversation was too long to complete in the allowed iterations.",
            is_error=True,
        ),
        None,
    )


async def run_query(
    context: QueryContext,
    messages: list[ConversationMessage],
) -> tuple[list[ConversationMessage], AsyncIterator[tuple[StreamEvent, UsageSnapshot | None]]]:
    """Run the conversation loop until the model stops requesting tools.

    Auto-compaction is checked at the start of each turn.  When the
    estimated token count exceeds the model's auto-compact threshold,
    the engine first tries a cheap microcompact (clearing old tool result
    content) and, if that is not enough, performs a full LLM-based
    summarization of older messages.

    Returns:
        (messages, event_stream) — messages is the updated conversation history
        (may be a new list after compaction), and event_stream yields
        (event, usage) tuples.
    """
    return messages, _run_query_loop(context, messages)


async def _execute_tool_call(
    context: QueryContext,
    tool_name: str,
    tool_use_id: str,
    tool_input: dict[str, object],
) -> ToolResultBlock:
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

    try:
        parsed_input = tool.input_model.model_validate(tool_input)
    except Exception as exc:
        return ToolResultBlock(
            tool_use_id=tool_use_id,
            content=f"Invalid input for {tool_name}: {exc}",
            is_error=True,
        )

    try:
        result = await tool.execute(
            parsed_input,
            ToolExecutionContext(
                cwd=context.cwd,
                metadata={
                    "tool_registry": context.tool_registry,
                    **(context.tool_metadata or {}),
                },
            ),
        )
    except Exception as exc:
        return ToolResultBlock(
            tool_use_id=tool_use_id,
            content=f"Tool execution failed: {exc}",
            is_error=True,
        )

    tool_result = ToolResultBlock(
        tool_use_id=tool_use_id,
        content=result.output,
        is_error=result.is_error,
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
