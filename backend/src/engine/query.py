"""Core tool-aware query loop."""

from __future__ import annotations

import asyncio
import logging
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
from engine.background_tasks import BackgroundTaskManager
from engine.stream_events import (
    AssistantTextDelta,
    AssistantTurnComplete,
    BackgroundTaskCompleted,
    BackgroundTaskStarted,
    StreamEvent,
    ThinkingDelta,
    ToolExecutionCancelled,
    ToolExecutionCompleted,
    ToolExecutionStarted,
)
from engine.streaming_executor import StreamingToolExecutor
from hooks import HookEvent, HookExecutor
from tools.base import ToolExecutionContext, ToolRegistry

logger = logging.getLogger(__name__)


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
    enable_background_tasks: bool = False


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

    background_manager: BackgroundTaskManager | None = None
    if context.enable_background_tasks:
        background_manager = BackgroundTaskManager()
        if context.tool_metadata is None:
            context.tool_metadata = {}
        context.tool_metadata["background_task_manager"] = background_manager

        # Register background tools
        from tools.builtins.check_background_progress import CheckBackgroundProgressTool
        from tools.builtins.cancel_background_task import CancelBackgroundTaskTool

        context.tool_registry.register(CheckBackgroundProgressTool())
        context.tool_registry.register(CancelBackgroundTaskTool())

    for _ in range(context.max_turns):
        messages, was_compacted = await auto_compact_if_needed(
            messages,
            api_client=context.api_client,
            model=context.model,
            system_prompt=context.system_prompt,
            state=compact_state,
        )

        # Inject completed background task results
        if background_manager is not None:
            for completed_task in background_manager.collect_completed():
                output = completed_task.result.output if completed_task.result else "No output"
                if len(output) > 2000:
                    output = f"[truncated, showing last 2000 chars]\n...{output[-2000:]}"
                status_label = (
                    "ERROR"
                    if (completed_task.result and completed_task.result.is_error)
                    else "COMPLETED"
                )
                messages.append(
                    ConversationMessage.from_user_text(
                        f"[BACKGROUND TASK {status_label}] {completed_task.tool_name} "
                        f"(task_id: {completed_task.task_id})\n\n{output}"
                    )
                )
                yield (
                    BackgroundTaskCompleted(
                        task_id=completed_task.task_id,
                        tool_name=completed_task.tool_name,
                        output=output,
                        is_error=completed_task.result.is_error if completed_task.result else False,
                    ),
                    None,
                )

        executor = StreamingToolExecutor(
            tool_registry=context.tool_registry,
            context=ToolExecutionContext(
                cwd=context.cwd,
                metadata=context.tool_metadata or {},
            ),
        )

        # Inject Daytona sandbox into context if DaytonaToolkit is registered
        # and a sandbox_id is configured. Gracefully skip if no sandbox is
        # available (toolkit may be registered for schema purposes only).
        daytona_toolkit = context.tool_registry.get_toolkit("sandbox_operations")
        if daytona_toolkit is not None and getattr(daytona_toolkit, "sandbox_id", None):
            try:
                await daytona_toolkit.prepare_context_async(executor._context)
                # Propagate sandbox metadata so fallback _execute_tool_call can find it
                if context.tool_metadata is None:
                    context.tool_metadata = {}
                context.tool_metadata.update(executor._context.metadata)
            except Exception:
                import logging

                logging.getLogger(__name__).debug(
                    "Sandbox context injection skipped (sandbox may not be configured)",
                    exc_info=True,
                )

        final_message: ConversationMessage | None = None
        usage = UsageSnapshot()
        pending_cancel: dict[str, str] = {}

        # Ephemeral background reminder — included in the API request
        # but NOT persisted in conversation history.  Purely informational
        # so the LLM knows background tasks exist; it can ignore this
        # freely when focused on foreground work.
        api_messages = messages
        if background_manager is not None and background_manager.has_pending():
            pending = [
                t for t in background_manager._tasks.values()
                if t.status == "running"
            ]
            if pending:
                import time as _time

                parts: list[str] = []
                for t in pending:
                    elapsed = _time.monotonic() - t.started_at
                    label = t.task_note or t.tool_name
                    header = f"Background: {label} ({elapsed:.0f}s) still running"
                    new_lines, since = background_manager.get_reminder_diff(t.task_id)
                    if new_lines:
                        logs = "\n".join(new_lines)
                        parts.append(
                            f"<system-reminder>\n{header}\n"
                            f"New output (last {len(new_lines)} lines):\n{logs}\n"
                            f"</system-reminder>"
                        )
                    else:
                        parts.append(
                            f"<system-reminder>\n{header}\n"
                            f"No new output in the last {since:.0f}s\n"
                            f"</system-reminder>"
                        )

                api_messages = list(messages) + [
                    ConversationMessage.from_user_text("\n".join(parts))
                ]

        async for event in context.api_client.stream_message(
            ApiMessageRequest(
                model=context.model,
                messages=api_messages,
                system_prompt=context.system_prompt,
                max_tokens=context.max_tokens,
                tools=context.tool_registry.to_api_schema(
                    inject_task_note=context.enable_background_tasks,
                ),
            )
        ):
            if isinstance(event, ApiThinkingDeltaEvent):
                logger.debug("STREAM: Received ApiThinkingDeltaEvent: text_len=%d", len(event.text))
                yield ThinkingDelta(text=event.text), None
                continue

            if isinstance(event, ApiTextDeltaEvent):
                if match := cancel_pattern.search(event.text):
                    tool_id, reason = match.groups()
                    pending_cancel[tool_id] = reason or "Cancelled by LLM"
                    logger.info(
                        "STREAM: Cancel pattern found in text: tool_id=%s reason=%s",
                        tool_id,
                        reason,
                    )
                logger.debug("STREAM: Received ApiTextDeltaEvent: text_len=%d", len(event.text))
                yield AssistantTextDelta(text=event.text), None
                continue

            if isinstance(event, ApiToolUseDeltaEvent):
                logger.info(
                    "STREAM: Received ApiToolUseDeltaEvent: id=%s name=%s input_keys=%s",
                    event.id,
                    event.name,
                    list(event.input.keys()) if event.input else None,
                )
                assistant_msg = final_message or ConversationMessage(role="assistant", content=[])
                started = executor.add_tool(event, assistant_msg)
                if started:
                    logger.info("STREAM: Yielding ToolExecutionStarted: name=%s", started.tool_name)
                    yield started, None
                for progress in executor.get_progress():
                    logger.debug(
                        "STREAM: Yielding ToolExecutionProgress: tool_id=%s", progress.tool_id
                    )
                    yield progress, None
                continue

            if isinstance(event, ApiCancelEvent):
                logger.info(
                    "STREAM: Received ApiCancelEvent: tool_id=%s reason=%s",
                    event.tool_id,
                    event.reason,
                )
                executor.cancel(event.tool_id, event.reason)
                continue

            if isinstance(event, ApiMessageCompleteEvent):
                logger.info(
                    "STREAM: Received ApiMessageCompleteEvent: tool_uses_count=%d",
                    len(event.message.tool_uses) if event.message.tool_uses else 0,
                )
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
            if background_manager is None or not background_manager.has_pending():
                return

            # Idle wait — no LLM turns, zero token cost
            completed_task = await background_manager.wait_any(timeout=300)

            if completed_task is not None:
                output = completed_task.result.output if completed_task.result else "No output"
                if len(output) > 2000:
                    output = f"[truncated, showing last 2000 chars]\n...{output[-2000:]}"
                status_label = (
                    "ERROR"
                    if (completed_task.result and completed_task.result.is_error)
                    else "COMPLETED"
                )
                messages.append(
                    ConversationMessage.from_user_text(
                        f"[BACKGROUND TASK {status_label}] {completed_task.tool_name} "
                        f"(task_id: {completed_task.task_id})\n\n{output}"
                    )
                )
                yield (
                    BackgroundTaskCompleted(
                        task_id=completed_task.task_id,
                        tool_name=completed_task.tool_name,
                        output=output,
                        is_error=completed_task.result.is_error if completed_task.result else False,
                    ),
                    None,
                )
            else:
                # Timeout — give LLM a turn to decide
                messages.append(
                    ConversationMessage.from_user_text(background_manager.compact_status())
                )
            continue

        # Yield started events for any remaining tools
        for started in executor.get_started_events():
            logger.info(
                "STREAM: Yielding (remaining) ToolExecutionStarted: name=%s", started.tool_name
            )
            yield started, None

        # Wait for all tools to complete and yield results
        tool_results: list[ToolResultBlock] = []
        for completed in executor.get_remaining():
            if isinstance(completed, ToolExecutionCompleted):
                logger.info(
                    "STREAM: Yielding ToolExecutionCompleted: name=%s is_error=%s output_len=%d",
                    completed.tool_name,
                    completed.is_error,
                    len(completed.output) if completed.output else 0,
                )
                tool_results.append(
                    ToolResultBlock(
                        tool_use_id="",  # filled by caller
                        content=completed.output,
                        is_error=completed.is_error,
                    )
                )
                yield completed, None
            elif isinstance(completed, ToolExecutionCancelled):
                logger.info(
                    "STREAM: Yielding ToolExecutionCancelled: name=%s reason=%s",
                    completed.tool_name,
                    completed.reason,
                )
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
            # Cancel any orphaned streaming executor tasks to avoid double
            # execution — the fallback below will re-execute them properly.
            executor.cancel_all()

            tool_calls = final_message.tool_uses
            foreground_calls = []

            for tc in tool_calls:
                # Strip meta-fields before passing input to the tool
                task_note = str(tc.input.pop("task_note", ""))
                is_background = tc.input.pop("background", False) if background_manager else False

                if is_background:
                    # Validate tool supports background
                    tool_def = context.tool_registry.get(tc.name)
                    if tool_def and not tool_def.supports_background:
                        tool_results.append(
                            ToolResultBlock(
                                tool_use_id=tc.id,
                                content=f"Tool '{tc.name}' does not support background execution.",
                                is_error=True,
                            )
                        )
                        yield (
                            ToolExecutionCompleted(
                                tool_name=tc.name,
                                output=f"Tool '{tc.name}' does not support background execution.",
                                is_error=True,
                            ),
                            None,
                        )
                        continue

                    # Launch background task — wrap _execute_tool_call to return ToolResult
                    async def _bg_wrapper(
                        ctx: QueryContext, name: str, uid: str, inp: dict[str, object]
                    ) -> "ToolResult":
                        from tools.base import ToolResult

                        block = await _execute_tool_call(ctx, name, uid, inp)
                        return ToolResult(output=block.content, is_error=block.is_error)

                    coro = _bg_wrapper(context, tc.name, tc.id, tc.input)
                    event = background_manager.launch(tc.id, tc.name, tc.input, coro, task_note=task_note)
                    yield event, None
                    tool_results.append(
                        ToolResultBlock(
                            tool_use_id=tc.id,
                            content=f"[BACKGROUND] Task launched. Task ID: {tc.id}. "
                            f"Use check_background_progress to monitor or "
                            f"cancel_background_task to stop it.",
                            is_error=False,
                        )
                    )
                else:
                    foreground_calls.append(tc)

            # Execute foreground tools (existing logic)
            if len(foreground_calls) == 1:
                tc = foreground_calls[0]
                logger.info(
                    "STREAM: Executing single foreground tool: name=%s id=%s", tc.name, tc.id
                )
                yield (
                    ToolExecutionStarted(
                        tool_name=tc.name,
                        tool_input=tc.input,
                    ),
                    None,
                )
                result = await _execute_tool_call(context, tc.name, tc.id, tc.input)
                tool_results.append(result)
                yield (
                    ToolExecutionCompleted(
                        tool_name=tc.name,
                        output=result.content,
                        is_error=result.is_error,
                    ),
                    None,
                )
            elif foreground_calls:
                logger.info(
                    "STREAM: Executing PARALLEL foreground tools: count=%d names=%s",
                    len(foreground_calls),
                    [tc.name for tc in foreground_calls],
                )
                started_events = []
                for tc in foreground_calls:
                    started_events.append(
                        ToolExecutionStarted(
                            tool_name=tc.name,
                            tool_input=tc.input,
                        )
                    )
                    logger.info(
                        "STREAM: Yielding parallel ToolExecutionStarted: name=%s id=%s",
                        tc.name,
                        tc.id,
                    )
                    yield started_events[-1], None

                logger.debug(
                    "STREAM: Launching asyncio.gather for %d parallel tools", len(foreground_calls)
                )
                results = await asyncio.gather(
                    *[
                        _execute_tool_call(context, tc.name, tc.id, tc.input)
                        for tc in foreground_calls
                    ]
                )
                logger.info("STREAM: All parallel tools completed, gathering results")
                tool_results.extend(results)
                for tc, result in zip(foreground_calls, results):
                    logger.info(
                        "STREAM: Yielding parallel ToolExecutionCompleted: name=%s is_error=%s output_len=%d",
                        tc.name,
                        result.is_error,
                        len(result.content) if result.content else 0,
                    )
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

    # Cleanup background tasks
    if background_manager is not None:
        background_manager.cancel_all()

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
