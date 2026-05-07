"""Tool dispatch coordination for assistant responses."""

from __future__ import annotations

import asyncio
import logging
from dataclasses import dataclass, field
from typing import TYPE_CHECKING

from engine.core.streaming_executor import StreamingToolExecutor, defer_background_dispatch
from engine.core.tool_batch import validate_tool_batch
from engine.runtime.background_dispatch import launch_and_collect_bg_events
from engine.runtime.background_tasks import BackgroundTaskManager
from message.messages import ConversationMessage, ToolResultBlock, ToolUseBlock
from message.stream_events import (
    StreamEvent,
    ToolExecutionCancelled,
    ToolExecutionCompleted,
)
from providers.types import UsageSnapshot
from tools import execute_tool_call_streaming

if TYPE_CHECKING:
    from engine.core.query import QueryContext


logger = logging.getLogger(__name__)


@dataclass(frozen=True)
class ToolDispatchResult:
    tool_results: list[ToolResultBlock]
    events: list[tuple[StreamEvent, UsageSnapshot | None]] = field(default_factory=list)


def _result_from_completed(completed: ToolExecutionCompleted) -> ToolResultBlock:
    return ToolResultBlock(
        tool_use_id=completed.tool_id,
        content=completed.output,
        is_error=completed.is_error,
        metadata=dict(completed.metadata or {}),
        does_terminate=completed.does_terminate,
    )


def _result_from_cancelled(completed: ToolExecutionCancelled) -> ToolResultBlock:
    return ToolResultBlock(
        tool_use_id=completed.tool_id,
        content=f"[CANCELLED] {completed.reason}",
        is_error=True,
    )


def _assign_missing_tool_result_ids(
    tool_results: list[ToolResultBlock],
    tool_uses: list[ToolUseBlock],
) -> None:
    assigned_ids: set[str] = {tr.tool_use_id for tr in tool_results if tr.tool_use_id}
    unassigned_ids = [tu.id for tu in tool_uses if tu.id not in assigned_ids]
    for result in tool_results:
        if not result.tool_use_id and unassigned_ids:
            result.tool_use_id = unassigned_ids.pop(0)


async def dispatch_assistant_tools(
    context: QueryContext,
    messages: list[ConversationMessage],
    final_message: ConversationMessage,
    executor: StreamingToolExecutor,
    *,
    streamed_rejections: list[ToolResultBlock],
    streamed_tool_use_ids: set[str],
    background_manager: BackgroundTaskManager | None,
) -> ToolDispatchResult:
    events: list[tuple[StreamEvent, UsageSnapshot | None]] = []
    tool_results: list[ToolResultBlock] = list(streamed_rejections)

    remaining_events = await executor.get_remaining()
    events.extend((emitted, None) for emitted in executor.get_events())
    for completed in remaining_events:
        if isinstance(completed, ToolExecutionCompleted):
            tool_results.append(_result_from_completed(completed))
            events.append((completed, None))
        elif isinstance(completed, ToolExecutionCancelled):
            tool_results.append(_result_from_cancelled(completed))
            events.append((completed, None))

    deferred_bg = executor.deferred_dispatch_ids
    if deferred_bg and background_manager is not None:
        for tc in final_message.tool_uses:
            if tc.id not in deferred_bg:
                continue
            tool_def_for_check = context.tool_registry.get(tc.name)
            if not defer_background_dispatch(tool_def_for_check, tc.input):
                continue
            events.extend(
                launch_and_collect_bg_events(
                    context,
                    messages,
                    background_manager,
                    tc,
                    tool_results,
                )
            )

    if not tool_results:
        executor.cancel_all()
        events.extend(
            await _dispatch_deferred_tool_calls(
                context,
                messages,
                final_message.tool_uses,
                streamed_tool_use_ids=streamed_tool_use_ids,
                background_manager=background_manager,
                tool_results=tool_results,
            )
        )

    _assign_missing_tool_result_ids(tool_results, final_message.tool_uses)
    return ToolDispatchResult(tool_results=tool_results, events=events)


async def _dispatch_deferred_tool_calls(
    context: QueryContext,
    messages: list[ConversationMessage],
    tool_calls: list[ToolUseBlock],
    *,
    streamed_tool_use_ids: set[str],
    background_manager: BackgroundTaskManager | None,
    tool_results: list[ToolResultBlock],
) -> list[tuple[StreamEvent, UsageSnapshot | None]]:
    events: list[tuple[StreamEvent, UsageSnapshot | None]] = []
    batch_rejection = validate_tool_batch(context, tool_calls)
    if batch_rejection is not None:
        tool_results.extend(batch_rejection)
        for tc, result in zip(tool_calls, batch_rejection, strict=True):
            events.append(
                (
                    ToolExecutionCompleted(
                        tool_name=tc.name,
                        output=result.content,
                        is_error=result.is_error,
                        tool_id=tc.id,
                        metadata=dict(result.metadata or {}),
                        does_terminate=result.does_terminate,
                    ),
                    None,
                )
            )
        return events

    foreground_calls: list[ToolUseBlock] = []
    for tc in tool_calls:
        tool_def_for_check = context.tool_registry.get(tc.name)
        force_bg = getattr(tool_def_for_check, "background", "forbidden") == "always"
        is_background = (
            (tc.input.get("background", False) or force_bg)
            if background_manager
            else False
        )

        if is_background:
            assert background_manager is not None
            events.extend(
                launch_and_collect_bg_events(
                    context,
                    messages,
                    background_manager,
                    tc,
                    tool_results,
                )
            )
        else:
            foreground_calls.append(tc)

    if len(foreground_calls) == 1:
        events.extend(
            await _dispatch_single_foreground_tool(
                context,
                messages,
                foreground_calls[0],
                streamed_tool_use_ids=streamed_tool_use_ids,
                tool_results=tool_results,
            )
        )
    elif foreground_calls:
        events.extend(
            await _dispatch_many_foreground_tools(
                context,
                messages,
                foreground_calls,
                streamed_tool_use_ids=streamed_tool_use_ids,
                tool_results=tool_results,
            )
        )
    return events


async def _dispatch_single_foreground_tool(
    context: QueryContext,
    messages: list[ConversationMessage],
    tc: ToolUseBlock,
    *,
    streamed_tool_use_ids: set[str],
    tool_results: list[ToolResultBlock],
) -> list[tuple[StreamEvent, UsageSnapshot | None]]:
    emitted_events: list[StreamEvent] = []

    async def emit(event: StreamEvent) -> None:
        emitted_events.append(event)

    result = await execute_tool_call_streaming(
        context,
        tc.name,
        tc.id,
        tc.input,
        emit=emit,
        conversation_messages=messages,
        consume_budget=tc.id not in streamed_tool_use_ids,
    )
    tool_results.append(result)
    events: list[tuple[StreamEvent, UsageSnapshot | None]] = [
        (emitted, None) for emitted in emitted_events
    ]
    events.append(
        (
            ToolExecutionCompleted(
                tool_name=tc.name,
                output=result.content,
                is_error=result.is_error,
                tool_id=tc.id,
                metadata=dict(result.metadata or {}),
                does_terminate=result.does_terminate,
            ),
            None,
        )
    )
    return events


async def _dispatch_many_foreground_tools(
    context: QueryContext,
    messages: list[ConversationMessage],
    foreground_calls: list[ToolUseBlock],
    *,
    streamed_tool_use_ids: set[str],
    tool_results: list[ToolResultBlock],
) -> list[tuple[StreamEvent, UsageSnapshot | None]]:
    queue: asyncio.Queue[StreamEvent | tuple[ToolUseBlock, ToolResultBlock]] = asyncio.Queue()
    events: list[tuple[StreamEvent, UsageSnapshot | None]] = []

    async def run_foreground(tc: ToolUseBlock) -> None:
        async def emit(event: StreamEvent) -> None:
            await queue.put(event)

        try:
            result = await execute_tool_call_streaming(
                context,
                tc.name,
                tc.id,
                tc.input,
                emit=emit,
                conversation_messages=messages,
                consume_budget=tc.id not in streamed_tool_use_ids,
            )
        except Exception as exc:
            logger.exception(
                "Foreground tool dispatch failed: tool_id=%s tool_name=%s",
                tc.id,
                tc.name,
            )
            result = ToolResultBlock(
                tool_use_id=tc.id,
                content=f"Tool execution failed: {exc}",
                is_error=True,
            )
        await queue.put((tc, result))

    tasks = [asyncio.create_task(run_foreground(tc)) for tc in foreground_calls]
    remaining = len(tasks)
    while remaining:
        item = await queue.get()
        if isinstance(item, tuple):
            tc, result = item
            tool_results.append(result)
            remaining -= 1
            events.append(
                (
                    ToolExecutionCompleted(
                        tool_name=tc.name,
                        output=result.content,
                        is_error=result.is_error,
                        tool_id=tc.id,
                        metadata=dict(result.metadata or {}),
                        does_terminate=result.does_terminate,
                    ),
                    None,
                )
            )
        else:
            events.append((item, None))
    await asyncio.gather(*tasks)
    return events
