"""Core tool-aware query loop."""

from __future__ import annotations

import asyncio
import logging
import re
import time as time_module
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING, Any
from collections.abc import AsyncIterator

if TYPE_CHECKING:
    from compaction import SessionState

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
from message.messages import (
    BackgroundTaskStateBlock,
    ConversationMessage,
    ToolResultBlock,
)
from engine.runtime.background_tasks import BackgroundTaskManager, TrackedBackgroundTask
from tools.daytona_toolkit.background import prepare_background_launch
from message.stream_events import (
    AssistantTextDelta,
    AssistantTurnComplete,
    BackgroundTaskCompleted,
    BackgroundTaskStarted,
    StreamEvent,
    SystemNotification,
    ThinkingDelta,
    ToolExecutionCancelled,
    ToolExecutionCompleted,
    ToolExecutionStarted,
)
from engine.core.notifications import build_budget_warning
from engine.core.streaming_executor import StreamingToolExecutor, defer_background_dispatch
from hooks import HookEvent, HookExecutor
from tools.core.base import (
    ExecutionMetadata,
    ToolExecutionContext,
    ToolRegistry,
    ToolResult,
    decorate_schemas_for_background,
    run_tool_safely,
)

logger = logging.getLogger(__name__)

MAX_OUTPUT_LENGTH: int = 2000
BACKGROUND_IDLE_TIMEOUT: int = 30  # Safety net — LLM should use wait_for_background_task explicitly
CANCEL_PATTERN = re.compile(r'\[CANCEL:(\S+)(?:\s+reason="([^"]*)")?\]')
_TOOL_TRACE_LIMIT = 64
_MERGED_RUNTIME_METADATA_KEYS = ("scope_packet", "coherence_token")


@dataclass
class QueryContext:
    api_client: SupportsStreamingMessages
    tool_registry: ToolRegistry
    cwd: Path
    model: str
    system_prompt: str
    max_tokens: int
    # Identity of the agent running this loop. Empty string for legacy
    # single-agent callers. Used by the stamping wrapper in :func:`run_query`
    # to tag every outgoing ``StreamEvent`` with ``agent_name`` / ``work_id``
    # so multi-agent printers can attribute events without sniffing context.
    agent_name: str = ""
    run_id: str = ""
    # Per-ephemeral-run cap on tool dispatches. ``None`` = unlimited. Each
    # spawned agent starts with a fresh ``tool_calls_used`` counter.
    tool_call_limit: int | None = None
    tool_calls_used: int = 0
    last_budget_warning_remaining: int | None = None
    hook_executor: HookExecutor | None = None
    tool_metadata: ExecutionMetadata | None = None
    session_state: SessionState | None = None
    enable_background_tasks: bool = False
    # Snapshot of the most recent api_messages list sent to the provider.
    # Updated by the query loop on every turn. Persistence layers read this
    # to populate the ``compacted_history`` column without having to re-run
    # compaction. ``None`` until the first turn completes.
    api_messages_snapshot: list[ConversationMessage] | None = None


def _ensure_execution_metadata(
    metadata: ExecutionMetadata | dict[str, object] | None,
) -> ExecutionMetadata:
    if isinstance(metadata, ExecutionMetadata):
        return metadata
    coerced = ExecutionMetadata()
    if metadata:
        coerced.update(metadata)
    return coerced


def _normalize_trace_paths(value: object) -> list[str]:
    if isinstance(value, str):
        stripped = value.strip()
        return [stripped] if stripped else []
    if isinstance(value, list):
        out: list[str] = []
        for item in value:
            if isinstance(item, str):
                stripped = item.strip()
                if stripped:
                    out.append(stripped)
        return out
    return []


def _append_trace_values(
    metadata: ExecutionMetadata | None,
    key: str,
    values: list[str],
) -> None:
    if metadata is None or not values:
        return
    existing = _normalize_trace_paths(metadata.get(key, []))
    seen = set(existing)
    for value in values:
        if value not in seen:
            existing.append(value)
            seen.add(value)
    if len(existing) > _TOOL_TRACE_LIMIT:
        existing = existing[-_TOOL_TRACE_LIMIT:]
    metadata[key] = existing


def _record_tool_trace(
    metadata: ExecutionMetadata | None,
    tool_name: str,
    tool_input: dict[str, object],
) -> None:
    if metadata is None:
        return
    if tool_name in {"ci_read_file", "daytona_read_file"}:
        _append_trace_values(
            metadata,
            "_read_paths_this_turn",
            _normalize_trace_paths(tool_input.get("path")),
        )
        return
    if tool_name != "run_subagent" or tool_input.get("agent_name") != "scout":
        return
    scout_input = tool_input.get("input")
    if not isinstance(scout_input, dict):
        return
    current_launches = metadata.get("_scout_launches_this_turn", 0)
    metadata["_scout_launches_this_turn"] = int(current_launches) + 1 if isinstance(current_launches, (int, float)) else 1
    _append_trace_values(
        metadata,
        "_scout_target_paths_this_turn",
        _normalize_trace_paths(scout_input.get("target_paths")),
    )


def _consume_tool_budget_or_reject(
    context: QueryContext,
    tool_use_id: str,
) -> ToolResultBlock | None:
    if context.tool_call_limit is None:
        return None
    if context.tool_calls_used >= context.tool_call_limit:
        return ToolResultBlock(
            tool_use_id=tool_use_id,
            content=(
                f"tool_call_limit exceeded: {context.tool_call_limit} tool "
                f"calls already used. The agent run will terminate after "
                f"this turn — call submit_summary / submit_plan now to "
                f"preserve partial work."
            ),
            is_error=True,
        )
    context.tool_calls_used += 1
    return None

def _deliver_completed_background_task(
    task: TrackedBackgroundTask,
    display_messages: list[ConversationMessage],
) -> BackgroundTaskCompleted:
    """Append a completion message to *display_messages* and return the event."""
    output = task.result.output if task.result else "No output"
    if task.tool_name != "run_subagent" and len(output) > MAX_OUTPUT_LENGTH:
        output = (
            f"[truncated, showing last {MAX_OUTPUT_LENGTH} chars]\n...{output[-MAX_OUTPUT_LENGTH:]}"
        )
    terminal_status = (
        "cancelled"
        if str(task.status) == "cancelled"
        else "failed"
        if str(task.status) == "failed"
        else "completed"
    )
    display_messages.append(
        ConversationMessage(
            role="user",
            content=[
                BackgroundTaskStateBlock(
                    task_id=task.task_id,
                    tool_name=task.tool_name,
                    task_type=task.task_type,
                    status=terminal_status,
                    source="engine_terminal",
                    text=output,
                    task_note=task.task_note,
                    run_id=task.run_id,
                    cancel_reason=task.cancel_reason,
                    completion_mode=getattr(task, "completion_mode", None),
                )
            ],
        )
    )
    return BackgroundTaskCompleted(
        task_id=task.task_id,
        tool_name=task.tool_name,
        output=output,
        is_error=task.result.is_error if task.result else False,
    )


def _append_and_emit_reminder(
    background_manager: BackgroundTaskManager,
    display_messages: list[ConversationMessage],
) -> SystemNotification | None:
    """Append a background reminder message and return the matching event.

    Returns ``None`` when no reminder is produced (no running tasks).
    The append + yield pairing is packaged here so the caller cannot
    drift the two sides apart.
    """
    reminder_msg = _build_background_reminder(background_manager)
    if reminder_msg is None:
        return None
    display_messages.append(reminder_msg)
    return SystemNotification(
        text=reminder_msg.background_task_state_text,
        category="background_progress",
    )


def _build_background_reminder(
    background_manager: BackgroundTaskManager,
) -> ConversationMessage | None:
    """Build a single durable user message summarising live background tasks.

    Returns ``None`` if no tasks are running. The returned message is a
    regular ``ConversationMessage`` and is appended to *display_messages*
    so the user (and subsequent compaction passes) can see it. It is NOT
    a separate ephemeral concept — once appended, it lives in history.

    Calling this advances the per-task reminder cursor via
    :meth:`BackgroundTaskManager.get_reminder_diff`, so each call yields
    only progress lines that have appeared since the previous reminder.
    """
    pending = list(background_manager.iter_running())
    if not pending:
        return None

    content: list[BackgroundTaskStateBlock] = []
    for t in pending:
        elapsed = time_module.monotonic() - t.started_at
        label = t.task_note or t.tool_name
        new_lines, since = background_manager.get_reminder_diff(t.task_id)
        if new_lines:
            text = f"Running for {elapsed:.0f}s\nNew output (last {len(new_lines)} lines):\n"
            text += "\n".join(new_lines)
        else:
            text = (
                f"Running for {elapsed:.0f}s\n"
                f"No new output in the last {since:.0f}s"
            )
        text += (
            "\nKeep working on any other ready analysis or tool tasks first. "
            "Only wait when this background task is the remaining blocker."
        )
        content.append(
            BackgroundTaskStateBlock(
                task_id=t.task_id,
                tool_name=t.tool_name,
                task_type=t.task_type,
                status="running",
                source="engine_progress",
                text=text,
                task_note=label,
                run_id=t.run_id,
            )
        )

    return ConversationMessage(role="user", content=content)


def _launch_background_tool(
    context: QueryContext,
    background_manager: BackgroundTaskManager,
    tool_use: object,  # ToolUseBlock; typed as object to avoid import cycles
    task_note: str,
) -> tuple[ToolResultBlock, BackgroundTaskStarted | None, ToolExecutionCompleted | None]:
    """Dispatch a single tool_use as a background task.

    Returns ``(tool_result_block, bg_event, reject_event)``:

    - ``tool_result_block`` — the block to add to the turn's tool_results.
    - ``bg_event`` — the ``BackgroundTaskStarted`` to yield, or ``None`` if
      the launch was rejected.
    - ``reject_event`` — a ``ToolExecutionCompleted`` to yield when the tool
      does not support background execution, otherwise ``None``.

    The caller is responsible for yielding whichever of the two events is
    non-``None`` so that tests and the router see a single ordered stream.
    """
    tc = tool_use  # local alias; attribute access below mirrors ToolUseBlock
    clean_input = {
        k: v for k, v in tc.input.items() if k not in ("background", "task_note")
    }

    tool_def = context.tool_registry.get(tc.name)
    if tool_def is None or getattr(tool_def, "background", "forbidden") == "forbidden":
        msg = f"Tool '{tc.name}' does not support background execution."
        return (
            ToolResultBlock(tool_use_id=tc.id, content=msg, is_error=True),
            None,
            ToolExecutionCompleted(tool_name=tc.name, output=msg, is_error=True),
        )

    sandbox = context.tool_metadata.daytona_sandbox if context.tool_metadata else None
    clean_input, kill_callback = prepare_background_launch(
        tc.name, clean_input, tc.id, sandbox
    )

    bg_alias = background_manager.next_alias()

    async def _bg_wrapper(
        ctx: QueryContext,
        name: str,
        uid: str,
        inp: dict[str, object],
        alias: str = bg_alias,
    ) -> ToolResult:
        bg_overrides = ExecutionMetadata(
            on_progress_line=background_manager.make_progress_callback(alias),
            background_task_id=alias,
        )
        block = await _execute_tool_call(
            ctx, name, uid, inp, extra_metadata=bg_overrides,
        )
        return ToolResult(output=block.content, is_error=block.is_error)

    coro = _bg_wrapper(context, tc.name, tc.id, clean_input)
    bg_event = background_manager.launch(
        bg_alias,
        tc.name,
        clean_input,
        coro,
        task_note=task_note,
        kill_callback=kill_callback,
        task_type=getattr(tool_def, "task_type", "agent"),
    )
    tool_result = ToolResultBlock(
        tool_use_id=tc.id,
        content=(
            f"[BACKGROUND LAUNCHED] task_id=\"{bg_alias}\" tool={tc.name}\n"
            f"Use this task_id with "
            f"check_background_progress(task_id=\"{bg_alias}\"), "
            f"wait_for_background_task(task_id=\"{bg_alias}\"), or "
            f"cancel_background_task(task_id=\"{bg_alias}\"). "
            f"Keep using the current turn on other ready work first; do not "
            f"wait immediately unless this task is the only blocker left. "
            f"A [BACKGROUND {bg_alias} COMPLETED] message will arrive automatically."
        ),
        is_error=False,
    )
    return tool_result, bg_event, None


async def _run_query_loop(
    context: QueryContext,
    display_messages: list[ConversationMessage],
) -> AsyncIterator[tuple[StreamEvent, UsageSnapshot | None]]:
    """Run the agentic tool loop.

    Two distinct message lists are maintained:

    - ``display_messages``: the append-only full history. Owned by the
      caller (EphemeralAgent / EvalAgent), persisted, and shown to the
      user. The query loop only **appends** to this list — it never
      mutates existing entries and never removes anything. Background
      reminders, completion notifications, assistant turns, and tool
      results all land here.
    - ``api_messages``: the compacted view sent to the LLM provider.
      Rebuilt fresh at the start of every turn from ``display_messages``
      via :func:`compact_for_api`. Never persisted, never returned. The
      reminder is part of ``display_messages`` so it is automatically
      reflected in the next ``api_messages`` snapshot.
    """
    from compaction import SessionState, compact_for_api

    compact_state = context.session_state or SessionState()
    context.tool_metadata = _ensure_execution_metadata(context.tool_metadata)

    background_manager: BackgroundTaskManager | None = None
    if context.enable_background_tasks:
        background_manager = BackgroundTaskManager()
        context.tool_metadata.background_task_manager = background_manager

    while True:
        streamed_rejections: list[ToolResultBlock] = []
        budget_warning = build_budget_warning(context)
        if budget_warning is not None:
            history_msg, warning_event = budget_warning
            display_messages.append(history_msg)
            yield warning_event, None

        if background_manager is not None:
            for completed_task in background_manager.collect_completed():
                event = _deliver_completed_background_task(completed_task, display_messages)
                yield event, None

            # Append a fresh background reminder to the durable history so
            # the user sees it AND the next compaction pass picks it up.
            if background_manager.has_pending():
                reminder_event = _append_and_emit_reminder(
                    background_manager, display_messages
                )
                if reminder_event is not None:
                    yield reminder_event, None

        executor = StreamingToolExecutor(
            tool_registry=context.tool_registry,
            context=ToolExecutionContext(
                cwd=context.cwd,
                metadata=context.tool_metadata,
            ),
            should_defer=(
                defer_background_dispatch if background_manager is not None else None
            ),
        )

        daytona_toolkit = context.tool_registry.get_toolkit("sandbox_operations")
        if (
            daytona_toolkit is None
            and context.tool_metadata is not None
            and context.tool_metadata.sandbox_id
            and (
                context.tool_registry.get_toolkit("code_intelligence") is not None
                or context.tool_registry.get_toolkit("atlas") is not None
            )
        ):
            try:
                from tools.daytona_toolkit import DaytonaToolkit

                daytona_toolkit = DaytonaToolkit(
                    sandbox_id=context.tool_metadata.sandbox_id
                )
            except Exception as exc:
                logger.debug(
                    "Temporary DaytonaToolkit creation skipped during CI context injection: %s",
                    exc,
                )

        if daytona_toolkit is not None and getattr(daytona_toolkit, "sandbox_id", None):
            try:
                await daytona_toolkit.prepare_context_async(executor._context)
                if context.tool_metadata is None:
                    context.tool_metadata = ExecutionMetadata()
                context.tool_metadata.update(executor._context.metadata)
            except Exception as exc:
                logger.debug(
                    "Sandbox context injection skipped (sandbox may not be configured): %s",
                    exc,
                )

        final_message: ConversationMessage | None = None
        usage = UsageSnapshot()
        pending_cancel: dict[str, str] = {}

        # Build the api_messages view fresh from display_messages every turn.
        # compact_for_api never mutates display_messages — the only list that
        # ever reaches the provider is api_messages.
        api_messages = await compact_for_api(
            display_messages,
            api_client=context.api_client,
            model=context.model,
            system_prompt=context.system_prompt,
            state=compact_state,
        )
        # Persistence + introspection hook: callers can read this AFTER the
        # loop returns to capture the final compacted view sent to the LLM.
        # The system prompt is prepended as a synthetic user-text message so
        # downstream token estimation reflects the full context size. The
        # system prompt itself is never compacted.
        context.api_messages_snapshot = [
            ConversationMessage.from_user_text(context.system_prompt),
            *api_messages,
        ]

        async for event in context.api_client.stream_message(
            ApiMessageRequest(
                model=context.model,
                messages=api_messages,
                system_prompt=context.system_prompt,
                max_tokens=context.max_tokens,
                tools=decorate_schemas_for_background(
                    context.tool_registry,
                    context.tool_registry.to_api_schema(),
                ) if context.enable_background_tasks
                else context.tool_registry.to_api_schema(),
            )
        ):
            if isinstance(event, ApiThinkingDeltaEvent):
                logger.debug("STREAM: Received ApiThinkingDeltaEvent: text_len=%d", len(event.text))
                yield ThinkingDelta(text=event.text), None
                continue

            if isinstance(event, ApiTextDeltaEvent):
                if match := CANCEL_PATTERN.search(event.text):
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
                budget_rejection = _consume_tool_budget_or_reject(context, event.id)
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

        for tool_id, reason in pending_cancel.items():
            executor.cancel(tool_id, reason)

        for progress in executor.get_progress():
            yield progress, None

        display_messages.append(final_message)
        yield AssistantTurnComplete(message=final_message, usage=usage), usage

        if not final_message.tool_uses:
            # The model produced no tool calls this turn. If we have no
            # background work either, we're done. Otherwise, idle-wait
            # briefly so a pending job can land and drive the next turn
            # instead of returning to the caller half-finished.
            if background_manager is None or not background_manager.has_pending():
                return

            completed_task = await background_manager.wait_any(timeout=BACKGROUND_IDLE_TIMEOUT)

            if completed_task is not None:
                event = _deliver_completed_background_task(completed_task, display_messages)
                yield event, None
            else:
                reminder_event = _append_and_emit_reminder(
                    background_manager, display_messages
                )
                if reminder_event is not None:
                    yield reminder_event, None
            continue

        for started in executor.get_started_events():
            logger.info(
                "STREAM: Yielding (remaining) ToolExecutionStarted: name=%s", started.tool_name
            )
            yield started, None

        tool_results: list[ToolResultBlock] = list(streamed_rejections)
        for completed in await executor.get_remaining():
            if isinstance(completed, ToolExecutionCompleted):
                logger.info(
                    "STREAM: Yielding ToolExecutionCompleted: name=%s is_error=%s output_len=%d",
                    completed.tool_name,
                    completed.is_error,
                    len(completed.output) if completed.output else 0,
                )
                tool_results.append(
                    ToolResultBlock(
                        tool_use_id=completed.tool_id,
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
                        tool_use_id=completed.tool_id,
                        content=f"[CANCELLED] {completed.reason}",
                        is_error=True,
                    )
                )
                yield completed, None

        # --- Launch background tools the streaming executor deferred ---
        deferred_bg = executor.deferred_dispatch_ids
        if deferred_bg and background_manager is not None:
            for tc in final_message.tool_uses:
                if tc.id not in deferred_bg:
                    continue
                task_note = str(tc.input.get("task_note", ""))
                tool_result, bg_event, reject_event = _launch_background_tool(
                    context, background_manager, tc, task_note
                )
                tool_results.append(tool_result)
                if bg_event is not None:
                    yield bg_event, None
                if reject_event is not None:
                    yield reject_event, None

        if not tool_results:
            executor.cancel_all()

            tool_calls = final_message.tool_uses
            foreground_calls = []

            for tc in tool_calls:
                task_note = str(tc.input.get("task_note", ""))
                tool_def_for_check = context.tool_registry.get(tc.name)
                force_bg = getattr(tool_def_for_check, "background", "forbidden") == "always"
                is_background = (
                    (tc.input.get("background", False) or force_bg)
                    if background_manager else False
                )

                if is_background:
                    tool_result, bg_event, reject_event = _launch_background_tool(
                        context, background_manager, tc, task_note
                    )
                    tool_results.append(tool_result)
                    if bg_event is not None:
                        yield bg_event, None
                    if reject_event is not None:
                        yield reject_event, None
                else:
                    foreground_calls.append(tc)

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
                        metadata=dict(result.metadata or {}),
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
                for tc, result in zip(foreground_calls, results, strict=True):
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
                            metadata=dict(result.metadata or {}),
                        ),
                        None,
                    )

        assigned_ids: set[str] = {tr.tool_use_id for tr in tool_results if tr.tool_use_id}
        unassigned_ids = [tu.id for tu in final_message.tool_uses if tu.id not in assigned_ids]
        for tr in tool_results:
            if not tr.tool_use_id and unassigned_ids:
                tr.tool_use_id = unassigned_ids.pop(0)

        display_messages.append(ConversationMessage(role="user", content=tool_results))  # type: ignore[arg-type]

        if _has_submission(context.tool_metadata):
            if background_manager is not None:
                await background_manager.cancel_all()
            return

        if (
            context.tool_call_limit is not None
            and context.tool_calls_used >= context.tool_call_limit
        ):
            if background_manager is not None:
                await background_manager.cancel_all()
            yield (
                ToolExecutionCompleted(
                    tool_name="",
                    output=(
                        f"Agent stopped: tool_call_limit "
                        f"({context.tool_call_limit}) exceeded."
                    ),
                    is_error=True,
                ),
                None,
            )
            return

_STAMPABLE_FIELDS = ("agent_name", "work_id")


def _stamp_event(
    event: StreamEvent,
    agent_name: str,
    work_id: str,
) -> StreamEvent:
    """Return *event* with empty ``agent_name`` / ``work_id`` filled in.

    Uses ``dataclasses.replace`` on frozen event dataclasses. No-op when the
    event already carries both fields or when the context has nothing to
    stamp (single-agent callers that never set ``QueryContext.agent_name``).
    """
    from dataclasses import fields, is_dataclass, replace

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


async def _stamped_stream(
    inner: AsyncIterator[tuple[StreamEvent, UsageSnapshot | None]],
    agent_name: str,
    work_id: str,
) -> AsyncIterator[tuple[StreamEvent, UsageSnapshot | None]]:
    async for event, usage in inner:
        yield _stamp_event(event, agent_name, work_id), usage


async def run_query(
    context: QueryContext,
    display_messages: list[ConversationMessage],
) -> tuple[list[ConversationMessage], AsyncIterator[tuple[StreamEvent, UsageSnapshot | None]]]:
    """Run an agent loop against *display_messages*.

    The same list is returned so callers retain a reference to the
    append-only display history. The query loop appends to it in place;
    callers must not assume immutability.

    Every outgoing ``StreamEvent`` is stamped with ``context.agent_name`` /
    ``context.run_id`` (as ``work_id``) when those fields are empty, so
    multi-agent printers can attribute events without touching QueryContext.

    The compacted ``api_messages`` view is built fresh inside the loop and
    sent to the LLM provider — it is never returned. See
    :func:`compact_for_api`.
    """
    return display_messages, _stamped_stream(
        _run_query_loop(context, display_messages),
        context.agent_name,
        context.run_id,
    )


async def _execute_tool_call(
    context: QueryContext,
    tool_name: str,
    tool_use_id: str,
    tool_input: dict[str, object],
    extra_metadata: ExecutionMetadata | dict[str, Any] | None = None,
) -> ToolResultBlock:
    budget_rejection = _consume_tool_budget_or_reject(context, tool_use_id)
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
    if extra_metadata:
        metadata.update(extra_metadata)

    result = await run_tool_safely(
        tool,
        tool_input,
        ToolExecutionContext(cwd=context.cwd, metadata=metadata),
    )
    _merge_submission_metadata(
        original=context.tool_metadata,
        updated=metadata,
        result_metadata=result.metadata,
    )
    if not result.is_error:
        _record_tool_trace(context.tool_metadata, tool_name, tool_input)

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


def _merge_submission_metadata(
    *,
    original: ExecutionMetadata | None,
    updated: ExecutionMetadata,
    result_metadata: dict[str, Any] | None = None,
) -> None:
    """Propagate selected tool metadata back to the live metadata bag."""
    if original is None:
        return
    for key, value in updated.extras.items():
        if key.startswith("submitted_") and value is not None:
            original[key] = value
    for key in _MERGED_RUNTIME_METADATA_KEYS:
        value = updated.extras.get(key)
        if value is None and isinstance(result_metadata, dict):
            value = result_metadata.get(key)
        if value is not None:
            original[key] = value


def _has_submission(metadata: ExecutionMetadata | None) -> bool:
    """True when a submit tool has accepted a terminal payload for this run."""
    if metadata is None:
        return False
    return any(
        key.startswith("submitted_") and value is not None
        for key, value in metadata.extras.items()
    )
