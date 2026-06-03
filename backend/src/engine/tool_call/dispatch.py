"""Tool dispatch coordination for assistant responses."""

from __future__ import annotations

import asyncio
import logging
import os
from collections import Counter
from dataclasses import dataclass, field
from typing import TYPE_CHECKING

from engine.tool_call.phase_buffer import (
    finish_phase_buffer,
    start_phase_buffer,
)
from engine.tool_call.streaming import StreamingToolExecutor
from engine.background.dispatch import dispatch_background_tool_call
from engine.background.policy import (
    is_engine_background_tool,
)
from engine.background.task_supervisor import BackgroundTaskSupervisor
from message.message import Message, ToolResultBlock, ToolUseBlock
from message.events import (
    StreamEvent,
    ToolExecutionCancelledEvent,
    ToolExecutionCompletedEvent,
)
from sandbox._shared.clock import monotonic_now
from sandbox._shared.models import Intent
from sandbox.audit.lifecycle import emit_lifecycle_batch_rejected
from sandbox.audit.schema import (
    ToolCallSection,
    build_tool_call_event,
    safe_emit,
)
from tools import ToolResult, execute_tool_call_streaming

if TYPE_CHECKING:
    from engine.query.context import QueryContext


logger = logging.getLogger(__name__)


# Phase 4 §AC6: process-local counter for lifecycle-batch rejections.
# Keyed by ``(lifecycle_tool, sibling_count_bucket)`` to stay cardinality-safe.
# ``agent_id`` is intentionally a structured-log dimension (audit event)
# rather than a counter label.
_LIFECYCLE_BATCH_REJECTION_COUNTERS: Counter[tuple[str, str]] = Counter()


def get_lifecycle_batch_rejection_counters() -> dict[tuple[str, str], int]:
    """Read-only snapshot of the lifecycle-batch rejection counter map."""
    return dict(_LIFECYCLE_BATCH_REJECTION_COUNTERS)


def reset_lifecycle_batch_rejection_counters() -> None:
    """Test helper: zero out the counter between assertions."""
    _LIFECYCLE_BATCH_REJECTION_COUNTERS.clear()


def _sibling_count_bucket(sibling_count: int) -> str:
    if sibling_count <= 0:
        return "0"
    if sibling_count == 1:
        return "1"
    if sibling_count == 2:
        return "2"
    return "3+"


def _emit_tool_call_started(tool_call: ToolUseBlock) -> None:
    safe_emit(
        build_tool_call_event(
            "tool_call.started",
            ToolCallSection(tool_use_id=tool_call.tool_use_id, tool_name=tool_call.name),
        ),
        lane="normal",
    )


def _emit_tool_call_phase_and_finished(
    tool_call: ToolUseBlock,
    *,
    total_ms: float,
    exit_status: str,
) -> None:
    decision = finish_phase_buffer(total_ms)
    if decision.flush:
        for entry in decision.phases:
            safe_emit(
                build_tool_call_event(
                    "tool_call.phase",
                    ToolCallSection(
                        tool_use_id=tool_call.tool_use_id,
                        tool_name=tool_call.name,
                        phase=entry.phase,
                        duration_ms=entry.duration_ms,
                    ),
                ),
                lane="sample",
            )
    safe_emit(
        build_tool_call_event(
            "tool_call.finished",
            ToolCallSection(
                tool_use_id=tool_call.tool_use_id,
                tool_name=tool_call.name,
                total_ms=total_ms,
                exit_status=exit_status,
                phase_totals_rollup=decision.rollup or None,
            ),
        ),
        lane="normal",
    )


def _exit_status_from_result(result: ToolResult | ToolResultBlock) -> str:
    return "error" if getattr(result, "is_error", False) else "ok"


@dataclass(frozen=True)
class AssistantToolDispatchOutcome:
    tool_results: list[ToolResultBlock]
    terminal_result: ToolResult | None = None
    events: list[StreamEvent] = field(default_factory=list)


def _tool_result_block_from_completion(
    completed: ToolExecutionCompletedEvent,
) -> ToolResultBlock:
    return ToolResultBlock(
        tool_use_id=completed.tool_use_id,
        content=completed.output,
        is_error=completed.is_error,
        metadata=dict(completed.metadata or {}),
        is_terminal=completed.is_terminal,
    )


def _tool_result_block_from_cancellation(
    cancelled: ToolExecutionCancelledEvent,
) -> ToolResultBlock:
    return ToolResultBlock(
        tool_use_id=cancelled.tool_use_id,
        content=f"[CANCELLED] {cancelled.reason}",
        is_error=True,
    )


def _completion_event_from_tool_result_block(
    tool_call: ToolUseBlock,
    result: ToolResultBlock,
) -> ToolExecutionCompletedEvent:
    return ToolExecutionCompletedEvent(
        tool_name=tool_call.name,
        output=result.content,
        is_error=result.is_error,
        tool_use_id=tool_call.tool_use_id,
        metadata=dict(result.metadata or {}),
        is_terminal=result.is_terminal,
    )


def _first_terminal_tool_result(
    tool_results: list[ToolResultBlock],
) -> ToolResult | None:
    for result in tool_results:
        if not result.is_terminal:
            continue
        return ToolResult(
            output=str(result.content),
            is_error=result.is_error,
            metadata=dict(result.metadata or {}),
            is_terminal=True,
        )
    return None


def _validate_tool_batch(
    context: QueryContext,
    tool_calls: list[ToolUseBlock],
) -> list[ToolResultBlock] | None:
    if not tool_calls or len(tool_calls) <= 1:
        return None

    terminal_in_batch = [
        tool_call for tool_call in tool_calls if tool_call.name in context.terminal_tools
    ]
    if not terminal_in_batch:
        return None

    flagged_names = ", ".join(
        sorted({f"`{tool_call.name}`" for tool_call in terminal_in_batch})
    )
    called_names = ", ".join(f"`{tool_call.name}`" for tool_call in tool_calls)
    message = (
        f"Terminal tool {flagged_names} must be called alone. "
        f"This response batched it with other tools: {called_names}. "
        f"No tool in this batch executed. "
        f"Resubmit with only the exclusive tool in its own final batch."
    )
    return [
        ToolResultBlock(tool_use_id=str(tool_call.tool_use_id), content=message, is_error=True)
        for tool_call in tool_calls
    ]


def _record_tool_batch_rejection(
    context: QueryContext,
    tool_calls: list[ToolUseBlock],
    tool_results: list[ToolResultBlock],
) -> list[StreamEvent] | None:
    batch_rejection = _validate_tool_batch(context, tool_calls)
    if batch_rejection is None:
        return None
    tool_results.extend(batch_rejection)
    return [
        _completion_event_from_tool_result_block(tool_call, result)
        for tool_call, result in zip(tool_calls, batch_rejection, strict=True)
    ]


def _intent_for_tool(
    context: QueryContext, tool_call: ToolUseBlock
) -> Intent | None:
    tool_def = context.tool_registry.get(tool_call.name)
    intent = getattr(tool_def, "intent", None)
    return intent if isinstance(intent, Intent) else None


def _record_lifecycle_batch_rejection(
    context: QueryContext,
    tool_calls: list[ToolUseBlock],
    tool_results: list[ToolResultBlock],
) -> tuple[list[StreamEvent], list[ToolUseBlock]] | None:
    """Engine-side ``Intent.LIFECYCLE`` batch policy (Phase 4 §E1/§E2).

    Returns ``None`` when the batch is single-call or contains no lifecycle
    tools — the caller proceeds as before. Otherwise emits rejection
    ``ToolResultBlock``s, records telemetry, and returns
    ``(events, dispatchable_tool_calls)``. The lifecycle invariant at
    ``docs/architecture/tools/isolated-workspace.html:166`` ("later tool
    calls observe the new routing state") becomes enforceable here:

    * **>1 LIFECYCLE in batch:** all lifecycle calls rejected; non-lifecycle
      siblings (if any) still dispatch — they observe whatever routing
      state preceded the batch.
    * **=1 LIFECYCLE + ≥1 sibling:** siblings rejected; the lifecycle call
      dispatches solo. Divergence from the terminal-tool precedent is
      deliberate (see Phase 4 plan §Design): forcing the lifecycle call to
      also retry would loop the agent indefinitely.
    """
    if not tool_calls or len(tool_calls) <= 1:
        return None
    lifecycle_calls: list[ToolUseBlock] = []
    non_lifecycle_calls: list[ToolUseBlock] = []
    for call in tool_calls:
        if _intent_for_tool(context, call) is Intent.LIFECYCLE:
            lifecycle_calls.append(call)
        else:
            non_lifecycle_calls.append(call)
    if not lifecycle_calls:
        return None
    audit_path = os.environ.get("EOS_WORKSPACE_LIFECYCLE_AUDIT_PATH")
    agent_id = _batch_agent_id(context)
    if len(lifecycle_calls) > 1:
        names = ", ".join(f"`{c.name}`" for c in lifecycle_calls)
        message = (
            f"Multiple lifecycle tools in one batch ({names}); engine "
            "cannot choose ordering. Resubmit each lifecycle call in its "
            "own batch."
        )
        rejected_pairs = [
            (call, ToolResultBlock(tool_use_id=str(call.tool_use_id), content=message, is_error=True))
            for call in lifecycle_calls
        ]
        remaining = non_lifecycle_calls
        for call in lifecycle_calls:
            _LIFECYCLE_BATCH_REJECTION_COUNTERS[(call.name, "multi_lifecycle")] += 1
            emit_lifecycle_batch_rejected(
                lifecycle_tool=call.name,
                sibling_tools=tuple(c.name for c in lifecycle_calls if c is not call),
                agent_id=agent_id,
                audit_path=audit_path,
            )
    else:
        lifecycle_call = lifecycle_calls[0]
        sibling_names = ", ".join(f"`{c.name}`" for c in non_lifecycle_calls)
        message = (
            f"`{lifecycle_call.name}` changes workspace routing; sibling "
            f"tools ({sibling_names}) were rejected to avoid ordering "
            "ambiguity. The lifecycle call executed. Resubmit the rejected "
            "tools in the next batch."
        )
        rejected_pairs = [
            (call, ToolResultBlock(tool_use_id=str(call.tool_use_id), content=message, is_error=True))
            for call in non_lifecycle_calls
        ]
        remaining = [lifecycle_call]
        bucket = _sibling_count_bucket(len(non_lifecycle_calls))
        _LIFECYCLE_BATCH_REJECTION_COUNTERS[(lifecycle_call.name, bucket)] += 1
        emit_lifecycle_batch_rejected(
            lifecycle_tool=lifecycle_call.name,
            sibling_tools=tuple(c.name for c in non_lifecycle_calls),
            agent_id=agent_id,
            audit_path=audit_path,
        )
    tool_results.extend(block for _, block in rejected_pairs)
    events: list[StreamEvent] = [
        _completion_event_from_tool_result_block(call, block)
        for call, block in rejected_pairs
    ]
    return events, remaining


def _batch_agent_id(context: QueryContext) -> str:
    """Best-effort agent_id for audit records — empty string if unknown."""
    metadata = getattr(context, "tool_metadata", None)
    candidate = getattr(metadata, "agent_id", None) or getattr(context, "agent_run_id", "")
    return str(candidate or "")


async def dispatch_assistant_tools(
    context: QueryContext,
    messages: list[Message],
    final_message: Message,
    executor: StreamingToolExecutor,
    *,
    streamed_tool_use_ids: set[str],
    background_tasks: BackgroundTaskSupervisor | None,
) -> AssistantToolDispatchOutcome:
    events: list[StreamEvent] = []
    tool_results: list[ToolResultBlock] = []

    remaining_events = await executor.get_remaining()
    events.extend(executor.get_events())
    for completed in remaining_events:
        if isinstance(completed, ToolExecutionCompletedEvent):
            tool_results.append(_tool_result_block_from_completion(completed))
            events.append(completed)
        elif isinstance(completed, ToolExecutionCancelledEvent):
            tool_results.append(_tool_result_block_from_cancellation(completed))
            events.append(completed)

    rejection_events = _record_tool_batch_rejection(
        context,
        final_message.tool_uses,
        tool_results,
    )
    if rejection_events is not None:
        executor.cancel_all()
        events.extend(rejection_events)
        return AssistantToolDispatchOutcome(
            tool_results=tool_results,
            terminal_result=_first_terminal_tool_result(tool_results),
            events=events,
        )

    resolved_ids = {result.tool_use_id for result in tool_results if result.tool_use_id}
    pending_tool_calls = [
        tool_call
        for tool_call in final_message.tool_uses
        if tool_call.tool_use_id not in resolved_ids
    ]
    if pending_tool_calls:
        events.extend(
            await _dispatch_deferred_tool_calls(
                context,
                messages,
                pending_tool_calls,
                streamed_tool_use_ids=streamed_tool_use_ids,
                background_tasks=background_tasks,
                tool_results=tool_results,
            )
        )

    return AssistantToolDispatchOutcome(
        tool_results=tool_results,
        terminal_result=_first_terminal_tool_result(tool_results),
        events=events,
    )


async def _dispatch_deferred_tool_calls(
    context: QueryContext,
    messages: list[Message],
    tool_calls: list[ToolUseBlock],
    *,
    streamed_tool_use_ids: set[str],
    background_tasks: BackgroundTaskSupervisor | None,
    tool_results: list[ToolResultBlock],
) -> list[StreamEvent]:
    events: list[StreamEvent] = []
    rejection_events = _record_tool_batch_rejection(context, tool_calls, tool_results)
    if rejection_events is not None:
        return rejection_events

    lifecycle_rejection = _record_lifecycle_batch_rejection(
        context, tool_calls, tool_results
    )
    if lifecycle_rejection is not None:
        rejection_events_lc, tool_calls = lifecycle_rejection
        events.extend(rejection_events_lc)
        if not tool_calls:
            return events

    foreground_tool_calls: list[ToolUseBlock] = []
    for tool_call in tool_calls:
        tool_def = context.tool_registry.get(tool_call.name)
        should_run_in_background = bool(
            background_tasks is not None
            and tool_def is not None
            and is_engine_background_tool(tool_def)
        )

        if should_run_in_background:
            assert background_tasks is not None
            events.extend(
                dispatch_background_tool_call(
                    context,
                    messages,
                    background_tasks,
                    tool_call,
                    tool_results,
                )
            )
        else:
            foreground_tool_calls.append(tool_call)

    if len(foreground_tool_calls) == 1:
        events.extend(
            await _dispatch_single_foreground_tool(
                context,
                messages,
                foreground_tool_calls[0],
                streamed_tool_use_ids=streamed_tool_use_ids,
                tool_results=tool_results,
            )
        )
    elif foreground_tool_calls:
        events.extend(
            await _dispatch_many_foreground_tools(
                context,
                messages,
                foreground_tool_calls,
                streamed_tool_use_ids=streamed_tool_use_ids,
                tool_results=tool_results,
            )
        )
    return events


async def _dispatch_single_foreground_tool(
    context: QueryContext,
    messages: list[Message],
    tool_call: ToolUseBlock,
    *,
    streamed_tool_use_ids: set[str],
    tool_results: list[ToolResultBlock],
) -> list[StreamEvent]:
    emitted_events: list[StreamEvent] = []

    async def emit(event: StreamEvent) -> None:
        emitted_events.append(event)

    start_phase_buffer(tool_use_id=tool_call.tool_use_id, tool_name=tool_call.name)
    _emit_tool_call_started(tool_call)
    started_at = monotonic_now()
    exit_status = "error"
    try:
        result = await execute_tool_call_streaming(
            context,
            tool_call.name,
            tool_call.tool_use_id,
            tool_call.input,
            emit=emit,
            conversation_messages=messages,
            consume_budget=tool_call.tool_use_id not in streamed_tool_use_ids,
        )
        exit_status = _exit_status_from_result(result)
    finally:
        total_ms = (monotonic_now() - started_at) * 1000.0
        _emit_tool_call_phase_and_finished(
            tool_call, total_ms=total_ms, exit_status=exit_status
        )

    tool_results.append(result)
    events: list[StreamEvent] = list(emitted_events)
    events.append(_completion_event_from_tool_result_block(tool_call, result))
    return events


async def _dispatch_many_foreground_tools(
    context: QueryContext,
    messages: list[Message],
    foreground_tool_calls: list[ToolUseBlock],
    *,
    streamed_tool_use_ids: set[str],
    tool_results: list[ToolResultBlock],
) -> list[StreamEvent]:
    queue: asyncio.Queue[StreamEvent | tuple[ToolUseBlock, ToolResultBlock]] = asyncio.Queue()
    events: list[StreamEvent] = []

    async def run_foreground_tool(tool_call: ToolUseBlock) -> None:
        async def emit(event: StreamEvent) -> None:
            await queue.put(event)

        start_phase_buffer(tool_use_id=tool_call.tool_use_id, tool_name=tool_call.name)
        _emit_tool_call_started(tool_call)
        started_at = monotonic_now()
        exit_status = "error"
        try:
            try:
                result = await execute_tool_call_streaming(
                    context,
                    tool_call.name,
                    tool_call.tool_use_id,
                    tool_call.input,
                    emit=emit,
                    conversation_messages=messages,
                    consume_budget=tool_call.tool_use_id not in streamed_tool_use_ids,
                )
                exit_status = _exit_status_from_result(result)
            except Exception as exc:
                logger.exception(
                    "Foreground tool dispatch failed: tool_use_id=%s tool_name=%s",
                    tool_call.tool_use_id,
                    tool_call.name,
                )
                result = ToolResultBlock(
                    tool_use_id=tool_call.tool_use_id,
                    content=f"Tool execution failed: {exc}",
                    is_error=True,
                )
        finally:
            total_ms = (monotonic_now() - started_at) * 1000.0
            _emit_tool_call_phase_and_finished(
                tool_call, total_ms=total_ms, exit_status=exit_status
            )
        await queue.put((tool_call, result))

    tasks = [
        asyncio.create_task(run_foreground_tool(tool_call))
        for tool_call in foreground_tool_calls
    ]
    remaining = len(tasks)
    while remaining:
        item = await queue.get()
        if isinstance(item, tuple):
            tool_call, result = item
            tool_results.append(result)
            remaining -= 1
            events.append(_completion_event_from_tool_result_block(tool_call, result))
        else:
            events.append(item)
    await asyncio.gather(*tasks, return_exceptions=True)
    return events
