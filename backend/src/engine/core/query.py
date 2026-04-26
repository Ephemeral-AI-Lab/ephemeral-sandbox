"""Core tool-aware query loop."""

from __future__ import annotations

import asyncio
import logging
import re
from collections.abc import AsyncIterator, Callable, Iterable
from dataclasses import dataclass, field
from enum import Enum
from pathlib import Path
from typing import Any

from agents.types import AgentDefinition, ModeDefinition
from providers.types import (
    ApiCancelEvent,
    ApiMessageCompleteEvent,
    ApiMessageRequest,
    ApiTextDeltaEvent,
    ApiThinkingDeltaEvent,
    ApiToolUseDeltaEvent,
    SupportsStreamingMessages,
    UsageSnapshot,
)
from message.messages import ConversationMessage, ToolResultBlock, ToolUseBlock
from message.stream_events import (
    AssistantTextDelta,
    AssistantTurnComplete,
    StreamEvent,
    ThinkingDelta,
    ToolExecutionCancelled,
    ToolExecutionCompleted,
)
from engine.core.provider_history import prepare_provider_messages
from engine.core.tool_batch import validate_tool_batch
from engine.core.streaming_executor import StreamingToolExecutor, defer_background_dispatch
from engine.runtime.background_dispatch import launch_and_collect_bg_events
from engine.runtime.background_tasks import BackgroundTaskManager
from engine.runtime.background_tasks import (
    append_background_reminder,
    deliver_completed_background_task,
)
from notification.budget import build_budget_warning
from notification.reminders import system_reminders_from_metadata
from prompt.prompt_report_recorder import PromptReportRecorder
from tools.core.base import (
    ExecutionMetadata,
    ToolExecutionContextService,
    ToolRegistry,
    decorate_schemas_for_background,
)
from tools.core.tool_execution import (
    _consume_tool_budget_or_reject,
    evaluate_mode_gate,
    execute_tool_call_streaming,
)


logger = logging.getLogger(__name__)

CANCEL_PATTERN = re.compile(r'\[CANCEL:(\S+)(?:\s+reason="([^"]*)")?\]')


class QueryExitReason(str, Enum):
    """Why the query loop exited."""

    TEXT_RESPONSE = "text_response"      # no tool_uses in response
    TOOL_STOP = "tool_stop"              # terminal tool succeeded
    RESOURCE_LIMIT = "resource_limit"    # budget exhausted or max_tokens


@dataclass
class QueryContext:
    api_client: SupportsStreamingMessages
    tool_registry: ToolRegistry
    cwd: Path
    model: str
    system_prompt: str
    max_tokens: int
    agent_name: str = ""
    run_id: str = ""
    tool_call_limit: int | None = None
    tool_calls_used: int = 0
    last_budget_warning_remaining: int | None = None
    tool_metadata: ExecutionMetadata | None = None
    enable_background_tasks: bool = False
    user_context_message: str | None = None
    on_turn: Callable[[list[ConversationMessage]], None] | None = None
    terminal_tools: set[str] = field(default_factory=set)
    exit_reason: QueryExitReason | None = None
    prompt_report_recorder: PromptReportRecorder | None = None
    terminal_nudge_retries_used: int = 0
    terminal_nudge_budget_extended: bool = False
    # Agent mode typestate (see docs/architecture/agent-mode-system-v1.md).
    # ``agent_def`` is the bound AgentDefinition; ``active_mode`` is the
    # currently-active ModeDefinition. Both are populated at spawn time when
    # an agent_def is supplied. The dispatcher reads ``active_mode`` to gate
    # tool calls; the mode-entry tools mutate it via the ``mode_transition``
    # field on their ToolResult.
    agent_def: AgentDefinition | None = None
    active_mode: ModeDefinition | None = None


MAX_TERMINAL_NUDGE_RETRIES = 3
TERMINAL_NUDGE_BUDGET_BONUS = 10


def build_terminal_nudge_text(terminal_tools: Iterable[str], attempt: int) -> str:
    tool_list = ", ".join(sorted(terminal_tools))
    return (
        "[terminal-tool reminder] Your previous turn ended without a terminal tool. "
        "Your next assistant message must contain exactly one terminal "
        f"tool call: {tool_list}. Do not call non-terminal tools or add narration. "
        "If a terminal payload was rejected, fix only the reported schema issue "
        "and resubmit. "
        f"(nudge {attempt}/{MAX_TERMINAL_NUDGE_RETRIES})"
    )


def _should_defer_stream_tool_dispatch(
    context: QueryContext,
    background_manager: BackgroundTaskManager | None,
) -> Callable[[Any | None, dict[str, Any] | None], bool]:
    exclusive_batch_seen = False

    def _defer(tool_def: Any | None, tool_input: dict[str, Any] | None) -> bool:
        nonlocal exclusive_batch_seen
        if background_manager is not None and defer_background_dispatch(tool_def, tool_input):
            return True
        if exclusive_batch_seen:
            return True
        tool_name = str(getattr(tool_def, "name", "") or "")
        # Terminal and mode-entry tools are batch-exclusive — they must not
        # execute mid-stream alongside siblings. Defer so validate_tool_batch
        # can enforce exclusivity after the full tool_uses list is known.
        is_terminal = bool(tool_name) and tool_name in context.terminal_tools
        is_mode_entry = bool(getattr(tool_def, "is_mode_entry_tool", False))
        if is_terminal or is_mode_entry:
            exclusive_batch_seen = True
            return True
        return False

    return _defer

# ---------------------------------------------------------------------------
# Query loop
# ---------------------------------------------------------------------------


def _prompt_report_recorder(context: QueryContext) -> PromptReportRecorder:
    if context.prompt_report_recorder is not None:
        return context.prompt_report_recorder
    metadata = context.tool_metadata
    context.prompt_report_recorder = PromptReportRecorder(
        metadata.get("prompt_report_messages_path") if metadata is not None else None,
        base_event=(
            {
                "agent_run_id": metadata.get("agent_run_id"),
                "agent": context.agent_name or metadata.get("agent_name"),
                "model": context.model,
            }
            if metadata is not None
            else {"agent": context.agent_name, "model": context.model}
        ),
    )
    return context.prompt_report_recorder


def _any_terminal_result(tool_results: list[ToolResultBlock]) -> bool:
    """True if any tool result in this turn carries does_terminate=True.

    The flag is stamped by tool execution when a tool with
    ``is_terminal_tool=True`` returned a non-error result, so the query loop no
    longer needs to re-derive that decision from tool names.
    """
    return any(result.does_terminate for result in tool_results)


async def _run_query_loop(
    context: QueryContext,
    display_messages: list[ConversationMessage],
) -> AsyncIterator[tuple[StreamEvent, UsageSnapshot | None]]:
    if context.tool_metadata is None:
        context.tool_metadata = ExecutionMetadata()
    elif not isinstance(context.tool_metadata, ExecutionMetadata):
        coerced = ExecutionMetadata()
        coerced.update(context.tool_metadata)
        context.tool_metadata = coerced

    background_manager: BackgroundTaskManager | None = None
    if context.enable_background_tasks:
        background_manager = BackgroundTaskManager()
        context.tool_metadata.background_task_manager = background_manager

    # Derive terminal tool names from the registry. Tools self-annotate via
    # ``is_terminal_tool=True``. The ``not pre-set`` guard lets test fixtures
    # construct ``QueryContext(terminal_tools={...})`` directly without
    # registering full tool implementations; in production this set is always
    # empty at this point and gets populated here.
    if not context.terminal_tools:
        context.terminal_tools = {
            tool.name
            for tool in context.tool_registry.list_tools()
            if getattr(tool, "is_terminal_tool", False)
        }

    while True:
        streamed_rejections: list[ToolResultBlock] = []
        budget_warning = build_budget_warning(context)
        if budget_warning is not None:
            history_msg, warning_event = budget_warning
            display_messages.append(history_msg)
            yield warning_event, None

        if background_manager is not None:
            for completed_task in background_manager.collect_completed():
                event = deliver_completed_background_task(completed_task, display_messages)
                yield event, None

            if background_manager.has_pending():
                append_background_reminder(background_manager, display_messages)

        if context.on_turn is not None:
            try:
                context.on_turn(display_messages)
            except Exception:
                logger.debug("on_turn callback failed", exc_info=True)

        executor = StreamingToolExecutor(
            tool_registry=context.tool_registry,
            context=ToolExecutionContextService(
                cwd=context.cwd,
                services=context.tool_metadata,
            ),
            should_defer=_should_defer_stream_tool_dispatch(
                context,
                background_manager=background_manager,
            ),
        )

        registered_tool_names = {tool.name for tool in context.tool_registry.list_tools()}
        sandbox_tool_names = {
            "delete_file",
            "edit_file",
            "glob",
            "grep",
            "move_file",
            "read_file",
            "shell",
            "write_file",
        }
        needs_sandbox_context = any(
            name in sandbox_tool_names or name.startswith("ci_")
            for name in registered_tool_names
        )
        if (
            context.tool_metadata is not None
            and context.tool_metadata.sandbox_id
            and needs_sandbox_context
        ):
            try:
                from tools.daytona_toolkit import DaytonaContextPreparer

                preparer = DaytonaContextPreparer(context.tool_metadata.sandbox_id)
                await preparer.prepare_context_async(executor._context)
                if context.tool_metadata is None:
                    context.tool_metadata = ExecutionMetadata()
                context.tool_metadata.update(executor._context.services_copy())
            except Exception as exc:
                logger.debug(
                    "Sandbox context injection skipped (sandbox may not be configured): %s",
                    exc,
                )

        final_message: ConversationMessage | None = None
        usage = UsageSnapshot()
        pending_cancel: dict[str, str] = {}
        streamed_tool_use_ids: set[str] = set()

        provider_messages = prepare_provider_messages(display_messages)
        context_message = (context.user_context_message or "").strip()
        if context_message:
            provider_messages = [
                ConversationMessage.from_user_text(context_message),
                *provider_messages,
            ]
        prompt_report = _prompt_report_recorder(context)
        prompt_report_seq = prompt_report.next_seq()
        tool_schemas = context.tool_registry.to_api_schema()
        if context.enable_background_tasks:
            tool_schemas = decorate_schemas_for_background(
                context.tool_registry,
                tool_schemas,
                terminal_tools=context.terminal_tools,
            )

        prompt_report.record(
            {
                "event": "llm_request",
                "seq": prompt_report_seq,
                "system_prompt": context.system_prompt,
                "user_context_message": context_message,
                "messages": [m.model_dump(mode="json") for m in provider_messages],
                "tools": tool_schemas,
            }
        )

        async for event in context.api_client.stream_message(
            ApiMessageRequest(
                model=context.model,
                messages=provider_messages,
                system_prompt=context.system_prompt,
                max_tokens=context.max_tokens,
                tools=tool_schemas,
            )
        ):
            if isinstance(event, ApiThinkingDeltaEvent):
                yield ThinkingDelta(text=event.text), None
                continue

            if isinstance(event, ApiTextDeltaEvent):
                if match := CANCEL_PATTERN.search(event.text):
                    tool_id, reason = match.groups()
                    pending_cancel[tool_id] = reason or "Cancelled by LLM"
                yield AssistantTextDelta(text=event.text), None
                continue

            if isinstance(event, ApiToolUseDeltaEvent):
                streamed_tool_use_ids.add(event.id)
                mode_rejection = evaluate_mode_gate(
                    context.active_mode,
                    event.name,
                    event.id,
                )
                if mode_rejection is not None:
                    streamed_rejections.append(mode_rejection)
                    yield (
                        ToolExecutionCompleted(
                            tool_name=event.name,
                            output=mode_rejection.content,
                            is_error=True,
                            tool_id=event.id,
                        ),
                        None,
                    )
                    continue
                budget_rejection = _consume_tool_budget_or_reject(
                    context,
                    event.name,
                    event.id,
                )
                if budget_rejection is not None:
                    streamed_rejections.append(budget_rejection)
                    yield (
                        ToolExecutionCompleted(
                            tool_name=event.name,
                            output=budget_rejection.content,
                            is_error=True,
                            tool_id=event.id,
                        ),
                        None,
                    )
                    continue
                executor.add_tool(event)
                for emitted in executor.get_events():
                    yield emitted, None
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

        for tool_id, reason in pending_cancel.items():
            executor.cancel(tool_id, reason)

        for progress in executor.get_progress():
            yield progress, None
        for emitted in executor.get_events():
            yield emitted, None

        display_messages.append(final_message)
        prompt_report.record(
            {
                "event": "assistant",
                "seq": prompt_report_seq,
                "message": final_message.model_dump(mode="json"),
                "usage": usage.model_dump(mode="json"),
            }
        )
        yield AssistantTurnComplete(message=final_message, usage=usage), usage

        if not final_message.tool_uses:
            if (
                context.terminal_tools
                and context.terminal_nudge_retries_used < MAX_TERMINAL_NUDGE_RETRIES
            ):
                context.terminal_nudge_retries_used += 1
                if (
                    context.tool_call_limit is not None
                    and not context.terminal_nudge_budget_extended
                ):
                    context.tool_call_limit += TERMINAL_NUDGE_BUDGET_BONUS
                    context.terminal_nudge_budget_extended = True
                attempt = context.terminal_nudge_retries_used
                nudge_text = build_terminal_nudge_text(context.terminal_tools, attempt)
                nudge_message = ConversationMessage.from_user_text(nudge_text)
                display_messages.append(nudge_message)
                prompt_report.record(
                    {
                        "event": "terminal_nudge",
                        "seq": prompt_report.next_seq(),
                        "attempt": attempt,
                        "message": nudge_message.model_dump(mode="json"),
                    }
                )
                continue
            if background_manager is None or not background_manager.has_pending():
                context.exit_reason = QueryExitReason.TEXT_RESPONSE
                return

            completed_task = await background_manager.wait_any(timeout=30)
            if completed_task is not None:
                event = deliver_completed_background_task(completed_task, display_messages)
                yield event, None
            else:
                append_background_reminder(background_manager, display_messages)
            continue

        tool_results: list[ToolResultBlock] = list(streamed_rejections)
        remaining_events = await executor.get_remaining()
        for emitted in executor.get_events():
            yield emitted, None
        for completed in remaining_events:
            if isinstance(completed, ToolExecutionCompleted):
                tool_results.append(
                    ToolResultBlock(
                        tool_use_id=completed.tool_id,
                        content=completed.output,
                        is_error=completed.is_error,
                        metadata=dict(completed.metadata or {}),
                        does_terminate=completed.does_terminate,
                        mode_transition=completed.mode_transition,
                    )
                )
                yield completed, None
            elif isinstance(completed, ToolExecutionCancelled):
                tool_results.append(
                    ToolResultBlock(
                        tool_use_id=completed.tool_id,
                        content=f"[CANCELLED] {completed.reason}",
                        is_error=True,
                    )
                )
                yield completed, None

        deferred_bg = executor.deferred_dispatch_ids
        if deferred_bg and background_manager is not None:
            for tc in final_message.tool_uses:
                if tc.id not in deferred_bg:
                    continue
                tool_def_for_check = context.tool_registry.get(tc.name)
                if not defer_background_dispatch(tool_def_for_check, tc.input):
                    continue
                for ev in launch_and_collect_bg_events(
                    context, background_manager, tc, tool_results
                ):
                    yield ev

        if not tool_results:
            executor.cancel_all()

            tool_calls = final_message.tool_uses
            batch_rejection = validate_tool_batch(context, tool_calls)
            if batch_rejection is not None:
                tool_results.extend(batch_rejection)
                for tc, result in zip(tool_calls, batch_rejection, strict=True):
                    yield (
                        ToolExecutionCompleted(
                            tool_name=tc.name,
                            output=result.content,
                            is_error=result.is_error,
                            metadata=dict(result.metadata or {}),
                        ),
                        None,
                    )
            else:
                foreground_calls = []

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
                        for ev in launch_and_collect_bg_events(
                            context, background_manager, tc, tool_results
                        ):
                            yield ev
                    else:
                        foreground_calls.append(tc)

                if len(foreground_calls) == 1:
                    tc = foreground_calls[0]
                    emitted_events: list[StreamEvent] = []

                    async def emit(event: StreamEvent) -> None:
                        emitted_events.append(event)

                    result = await execute_tool_call_streaming(
                        context,
                        tc.name,
                        tc.id,
                        tc.input,
                        emit=emit,
                        consume_budget=tc.id not in streamed_tool_use_ids,
                    )
                    tool_results.append(result)
                    for emitted in emitted_events:
                        yield emitted, None
                    yield (
                        ToolExecutionCompleted(
                            tool_name=tc.name,
                            output=result.content,
                            is_error=result.is_error,
                            metadata=dict(result.metadata or {}),
                        ),
                        None,
                    )
                elif foreground_calls:
                    queue: asyncio.Queue[StreamEvent | tuple[ToolUseBlock, ToolResultBlock]] = (
                        asyncio.Queue()
                    )

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
                            yield (
                                ToolExecutionCompleted(
                                    tool_name=tc.name,
                                    output=result.content,
                                    is_error=result.is_error,
                                    metadata=dict(result.metadata or {}),
                                ),
                                None,
                            )
                        else:
                            yield item, None
                    await asyncio.gather(*tasks)

        assigned_ids: set[str] = {tr.tool_use_id for tr in tool_results if tr.tool_use_id}
        unassigned_ids = [tu.id for tu in final_message.tool_uses if tu.id not in assigned_ids]
        for tr in tool_results:
            if not tr.tool_use_id and unassigned_ids:
                tr.tool_use_id = unassigned_ids.pop(0)

        tool_result_message = ConversationMessage(role="user", content=tool_results)
        display_messages.append(tool_result_message)
        prompt_report.record(
            {
                "event": "tool_result",
                "seq": prompt_report_seq,
                "message": tool_result_message.model_dump(mode="json"),
            }
        )
        system_reminders = []
        for result in tool_results:
            system_reminders.extend(system_reminders_from_metadata(dict(result.metadata or {})))
        if system_reminders:
            reminder_message = ConversationMessage(role="user", content=system_reminders)
            display_messages.append(reminder_message)
            prompt_report.record(
                {
                    "event": "hook_system_reminder",
                    "seq": prompt_report.next_seq(),
                    "message": reminder_message.model_dump(mode="json"),
                }
            )

        # Apply any mode transition reported by a mode-entry tool this turn.
        # Entry tools are batch-exclusive (validate_tool_batch enforces it),
        # so at most one transition fires per turn — the loop is defensive.
        if context.agent_def is not None:
            for tr in tool_results:
                if tr.mode_transition:
                    next_mode = context.agent_def.modes_by_name.get(tr.mode_transition)
                    if next_mode is not None:
                        context.active_mode = next_mode

        # Check for a successful terminal tool. A rejected terminal call
        # is feedback for the next model turn, not a completed terminal result.
        if _any_terminal_result(tool_results):
            context.exit_reason = QueryExitReason.TOOL_STOP
            return

        if (
            context.tool_call_limit is not None
            and context.tool_calls_used >= context.tool_call_limit
        ):
            context.exit_reason = QueryExitReason.RESOURCE_LIMIT
            if background_manager is not None:
                await background_manager.cancel_all()
            yield (
                ToolExecutionCompleted(
                    tool_name="",
                    output=f"Agent stopped: tool_call_limit ({context.tool_call_limit}) exceeded.",
                    is_error=True,
                ),
                None,
            )
            return


async def run_query(
    context: QueryContext,
    display_messages: list[ConversationMessage],
) -> tuple[list[ConversationMessage], AsyncIterator[tuple[StreamEvent, UsageSnapshot | None]]]:
    from dataclasses import fields, is_dataclass, replace

    agent_name = context.agent_name
    work_id = context.run_id

    def _stamp(
        event: StreamEvent,
    ) -> StreamEvent:
        if not is_dataclass(event):
            return event
        if not (agent_name or work_id):
            return event
        names = {f.name for f in fields(event)}
        updates: dict[str, str] = {}
        if "agent_name" in names and not getattr(event, "agent_name", ""):
            updates["agent_name"] = agent_name
        if "work_id" in names and not getattr(event, "work_id", ""):
            updates["work_id"] = work_id
        if not updates:
            return event
        return replace(event, **updates)

    async def _stamped(
        inner: AsyncIterator[tuple[StreamEvent, UsageSnapshot | None]],
    ) -> AsyncIterator[tuple[StreamEvent, UsageSnapshot | None]]:
        async for event, usage in inner:
            yield _stamp(event), usage

    return display_messages, _stamped(_run_query_loop(context, display_messages))
