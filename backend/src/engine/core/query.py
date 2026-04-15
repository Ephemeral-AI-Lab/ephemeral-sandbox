"""Core tool-aware query loop."""

from __future__ import annotations

import asyncio
import logging
import re
from collections.abc import AsyncIterator, Callable
from dataclasses import dataclass, field
from enum import Enum
from pathlib import Path
from typing import Any

from code_intelligence.editing.change_labels import change_actor_label
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
from message.messages import ConversationMessage, SystemReminderBlock, ToolResultBlock
from message.stream_events import (
    AssistantTextDelta,
    AssistantTurnComplete,
    StreamEvent,
    SystemNotification,
    ThinkingDelta,
    ToolExecutionCancelled,
    ToolExecutionCompleted,
    ToolExecutionStarted,
)
from engine.core.notifications import build_budget_warning
from engine.core.tool_batch import validate_tool_batch
from engine.core.streaming_executor import StreamingToolExecutor, defer_background_dispatch
from engine.runtime.background_dispatch import launch_and_collect_bg_events
from engine.runtime.background_tasks import BackgroundTaskManager
from engine.runtime.background_tasks import (
    append_and_emit_reminder,
    build_background_reminder,
    deliver_completed_background_task,
)
from tools.core.base import (
    ExecutionMetadata,
    ToolExecutionContext,
    ToolRegistry,
    decorate_schemas_for_background,
)
from tools.core.tool_execution import _consume_tool_budget_or_reject, execute_tool_call


logger = logging.getLogger(__name__)

CANCEL_PATTERN = re.compile(r'\[CANCEL:(\S+)(?:\s+reason="([^"]*)")?\]')


class QueryExitReason(str, Enum):
    """Why the query loop exited."""

    TEXT_RESPONSE = "text_response"      # no tool_uses in response
    TOOL_STOP = "tool_stop"              # terminal tool called
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
    hook_executor: Any = None
    tool_metadata: ExecutionMetadata | None = None
    session_state: Any = None
    enable_background_tasks: bool = False
    user_context_message: str | None = None
    on_turn: Callable[[list[ConversationMessage]], None] | None = None
    api_messages_snapshot: list[ConversationMessage] | None = None
    terminal_tools: set[str] = field(default_factory=set)
    exit_reason: QueryExitReason | None = None


def _should_defer_stream_tool_dispatch(
    context: QueryContext,
    background_manager: BackgroundTaskManager | None,
) -> Callable[[Any | None, dict[str, Any] | None], bool]:
    from tools.builtins.skills.toolkit import (
        get_reference_terminal_action,
        get_required_next_tool,
    )

    guarded_batch_seen = False

    def _defer(tool_def: Any | None, tool_input: dict[str, Any] | None) -> bool:
        nonlocal guarded_batch_seen
        if background_manager is not None and defer_background_dispatch(tool_def, tool_input):
            return True
        if get_required_next_tool(context.tool_metadata) is not None:
            return True
        if guarded_batch_seen:
            return True
        tool_name = str(getattr(tool_def, "name", "") or "")
        if get_reference_terminal_action(tool_name, tool_input):
            guarded_batch_seen = True
            return True
        return False

    return _defer

# Backward-compatibility aliases for internal test imports
_execute_tool_call = execute_tool_call
_build_background_reminder = build_background_reminder


# ---------------------------------------------------------------------------
# Scope-change auto-check (replaces ScopeChangeBuffer push system)
# ---------------------------------------------------------------------------

SCOPE_CHANGE_CATEGORY = "scope_change"
SCOPE_CHANGE_SUPERSEDED = "scope_change_superseded"

_MIN_TURNS_BETWEEN_NOTIFICATIONS = 1


def _scope_change_auto_check(
    metadata: ExecutionMetadata,
    display_messages: list[ConversationMessage],
) -> str | None:
    """Check arbiter history for scope changes by other agents.

    Called at the top of each query loop turn. Returns notification text
    when changes are found (and the turn-gate allows), otherwise None.
    Replaces the old ScopeChangeBuffer push system with a pull from the
    current arbiter history.
    """
    arbiter = metadata.get("arbiter")
    if arbiter is None or not getattr(arbiter, "initialized", False):
        return None

    scope_paths = metadata.get("write_scope") or []
    if not scope_paths:
        return None

    agent_run_id = metadata.get("agent_run_id", "")

    # Turn-gating state
    turn_state = metadata.extras.setdefault(
        "_scope_change_turn_state",
        {"turns_since_last_notification": 0},
    )
    turn_state["turns_since_last_notification"] += 1

    if turn_state["turns_since_last_notification"] < _MIN_TURNS_BETWEEN_NOTIFICATIONS:
        return None

    # Use the most recent baseline: auto-check, explicit tool check, or task start
    since = max(
        float(metadata.get("_auto_freshness_checked_at") or 0),
        float(metadata.get("freshness_checked_at") or 0),
        float(metadata.get("work_item_started_at") or 0),
    )

    changes = arbiter.changes_since(
        since,
        team_run_id=str(metadata.get("team_run_id") or "") or None,
    )
    # Filter to scope and exclude self
    relevant = [
        c for c in changes
        if c.agent_run_id != agent_run_id
        and any(c.file_path.startswith(p.rstrip("/")) for p in scope_paths)
    ]

    if not relevant:
        return None

    # Build notification text with per-file detail
    lines = [
        f"- {c.file_path} ({c.edit_type} by {change_actor_label(c)})"
        for c in relevant
    ]
    text = (
        "Files in your scope were edited by other agents. "
        "Re-read before editing:\n" + "\n".join(lines)
    )

    # Mark previous scope_change notification as superseded for compaction
    last_idx = metadata.extras.get("_scope_change_last_msg_idx")
    if last_idx is not None:
        try:
            old_msg = display_messages[last_idx]
            if (
                old_msg.content
                and hasattr(old_msg.content[0], "category")
                and old_msg.content[0].category == SCOPE_CHANGE_CATEGORY
            ):
                old_msg.content[0].category = SCOPE_CHANGE_SUPERSEDED
        except (IndexError, AttributeError):
            pass  # display_messages may have been compacted

    metadata.extras["_scope_change_last_msg_idx"] = len(display_messages)
    display_messages.append(
        ConversationMessage(
            role="user",
            content=[SystemReminderBlock(category=SCOPE_CHANGE_CATEGORY, text=text)],
        )
    )

    # Update baseline and reset turn counter
    import time
    metadata["_auto_freshness_checked_at"] = time.time()
    turn_state["turns_since_last_notification"] = 0

    logger.info(
        "[scope_auto_check] injected %d file change(s) into agent context",
        len(relevant),
    )
    return text


# ---------------------------------------------------------------------------
# Query loop
# ---------------------------------------------------------------------------


async def _run_query_loop(
    context: QueryContext,
    display_messages: list[ConversationMessage],
) -> AsyncIterator[tuple[StreamEvent, UsageSnapshot | None]]:
    from compaction import SessionState, compact_for_api

    compact_state = context.session_state or SessionState()
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

    # When a wait_for_background_task call is rejected (WAIT_TOO_EARLY /
    # WAIT_REQUIRES_PROGRESS_CHECK), the tool result already tells the
    # planner to back off. Emitting an engine_progress reminder on the
    # very next turn would contradict that rejection and mislead the
    # planner into thinking meaningful progress arrived. This flag
    # suppresses exactly one reminder cycle after such a rejection.
    suppress_bg_reminder: bool = False

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

            if background_manager.has_pending() and not suppress_bg_reminder:
                reminder_event = append_and_emit_reminder(background_manager, display_messages)
                if reminder_event is not None:
                    yield reminder_event, None
            suppress_bg_reminder = False

        if context.tool_metadata is not None:
            scope_notification = _scope_change_auto_check(context.tool_metadata, display_messages)
            if scope_notification is not None:
                yield SystemNotification(text=scope_notification, category="scope_change"), None

        if context.on_turn is not None:
            try:
                context.on_turn(display_messages)
            except Exception:
                logger.debug("on_turn callback failed", exc_info=True)

        executor = StreamingToolExecutor(
            tool_registry=context.tool_registry,
            context=ToolExecutionContext(
                cwd=context.cwd,
                metadata=context.tool_metadata,
            ),
            should_defer=_should_defer_stream_tool_dispatch(
                context,
                background_manager=background_manager,
            ),
        )

        daytona_toolkit = context.tool_registry.get_toolkit("sandbox_operations")
        if (
            daytona_toolkit is None
            and context.tool_metadata is not None
            and context.tool_metadata.sandbox_id
            and context.tool_registry.get_toolkit("code_intelligence") is not None
        ):
            try:
                from tools.daytona_toolkit import DaytonaToolkit

                daytona_toolkit = DaytonaToolkit(sandbox_id=context.tool_metadata.sandbox_id)
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

        api_messages = await compact_for_api(
            display_messages,
            api_client=context.api_client,
            model=context.model,
            system_prompt=context.system_prompt,
            state=compact_state,
        )
        provider_messages = list(api_messages)
        context_message = (context.user_context_message or "").strip()
        if context_message:
            provider_messages = [
                ConversationMessage.from_user_text(context_message),
                *provider_messages,
            ]
        context.api_messages_snapshot = [
            ConversationMessage.from_user_text(context.system_prompt),
            *(
                [ConversationMessage.from_user_text(context_message)]
                if context_message
                else []
            ),
            *api_messages,
        ]

        async for event in context.api_client.stream_message(
            ApiMessageRequest(
                model=context.model,
                messages=provider_messages,
                system_prompt=context.system_prompt,
                max_tokens=context.max_tokens,
                tools=decorate_schemas_for_background(
                    context.tool_registry,
                    context.tool_registry.to_api_schema(),
                )
                if context.enable_background_tasks
                else context.tool_registry.to_api_schema(),
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

        for tool_id, reason in pending_cancel.items():
            executor.cancel(tool_id, reason)

        for progress in executor.get_progress():
            yield progress, None

        display_messages.append(final_message)
        yield AssistantTurnComplete(message=final_message, usage=usage), usage

        if not final_message.tool_uses:
            if background_manager is None or not background_manager.has_pending():
                context.exit_reason = QueryExitReason.TEXT_RESPONSE
                return

            completed_task = await background_manager.wait_any(timeout=30)
            if completed_task is not None:
                event = deliver_completed_background_task(completed_task, display_messages)
                yield event, None
            else:
                reminder_event = append_and_emit_reminder(background_manager, display_messages)
                if reminder_event is not None:
                    yield reminder_event, None
            continue

        for started in executor.get_started_events():
            yield started, None

        tool_results: list[ToolResultBlock] = list(streamed_rejections)
        for completed in await executor.get_remaining():
            if isinstance(completed, ToolExecutionCompleted):
                tool_results.append(
                    ToolResultBlock(
                        tool_use_id=completed.tool_id,
                        content=completed.output,
                        is_error=completed.is_error,
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
                task_note = str(tc.input.get("task_note", ""))
                for ev in launch_and_collect_bg_events(
                    context, background_manager, tc, task_note, tool_results
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
                    task_note = str(tc.input.get("task_note", ""))
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
                            context, background_manager, tc, task_note, tool_results
                        ):
                            yield ev
                    else:
                        foreground_calls.append(tc)

                if len(foreground_calls) == 1:
                    tc = foreground_calls[0]
                    yield ToolExecutionStarted(tool_name=tc.name, tool_input=tc.input), None
                    result = await execute_tool_call(context, tc.name, tc.id, tc.input)
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
                    started_events = [
                        ToolExecutionStarted(tool_name=tc.name, tool_input=tc.input)
                        for tc in foreground_calls
                    ]
                    for ev in started_events:
                        yield ev, None

                    results = await asyncio.gather(
                        *[
                            execute_tool_call(context, tc.name, tc.id, tc.input)
                            for tc in foreground_calls
                        ]
                    )
                    tool_results.extend(results)
                    for tc, result in zip(foreground_calls, results, strict=True):
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

        display_messages.append(ConversationMessage(role="user", content=tool_results))

        # Check for terminal tool — exit immediately after tool results collected
        if context.terminal_tools:
            for tu in final_message.tool_uses:
                if tu.name in context.terminal_tools:
                    context.exit_reason = QueryExitReason.TOOL_STOP
                    return

        # Detect wait rejections so the next iteration suppresses the
        # engine_progress reminder that would otherwise contradict them.
        _WAIT_REJECTION_PREFIXES = ("[WAIT_TOO_EARLY]", "[WAIT_REQUIRES_PROGRESS_CHECK]")
        suppress_bg_reminder = any(
            tr.is_error and tr.content.startswith(_WAIT_REJECTION_PREFIXES)
            for tr in tool_results
        )

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
