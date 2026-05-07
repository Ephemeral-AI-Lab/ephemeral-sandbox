"""Core tool-aware query loop."""

from __future__ import annotations

import logging
import re
from collections.abc import AsyncIterator, Callable
from dataclasses import dataclass, field
from enum import Enum
from pathlib import Path
from typing import Any

from providers.types import (
    ApiCancelEvent,
    ApiMessageCompleteEvent,
    ApiTextDeltaEvent,
    ApiThinkingDeltaEvent,
    ApiToolUseDeltaEvent,
    SupportsStreamingMessages,
    UsageSnapshot,
)
from message.messages import ConversationMessage, ToolResultBlock
from message.stream_events import (
    AssistantMessageComplete,
    AssistantTextDelta,
    StreamEvent,
    ThinkingDelta,
    ToolExecutionCompleted,
)
from engine.core.notifications import (
    ensure_system_notification_service,
    flush_system_notifications,
)
from engine.core.streaming_executor import StreamingToolExecutor, defer_background_dispatch
from engine.core.tool_dispatch import dispatch_assistant_tools
from engine.core.tool_results import (
    any_terminal_result,
    terminal_result_from_tool_results,
)
from engine.core.run_request import (
    QueryRunRequest,
    build_query_run_request,
    record_assistant_message,
    record_tool_results,
)
from engine.runtime.background_tasks import BackgroundTaskManager
from engine.runtime.tool_context import prepare_tool_execution_context
from notification.rules import NotificationRule, dispatch_rules
from notification.service import SystemNotificationService
from prompt.prompt_report_recorder import PromptReportRecorder
from tools import (
    BaseTool,
    ExecutionMetadata,
    ToolResult,
    ToolExecutionContextService,
    ToolRegistry,
    _consume_tool_budget_or_reject,
)


logger = logging.getLogger(__name__)

CANCEL_PATTERN = re.compile(r'\[CANCEL:(\S+)(?:\s+reason="([^"]*)")?\]')


class QueryExitReason(str, Enum):
    """Why the query loop exited."""

    TEXT_RESPONSE = "text_response"      # no tool_uses in response
    TOOL_STOP = "tool_stop"              # terminal tool succeeded
    RESOURCE_LIMIT = "resource_limit"    # budget exhausted or max_tokens


@dataclass(frozen=True)
class _ToolBudgetView:
    """Read-only snapshot of tool-call budget state for notification rules."""

    used: int
    limit: int | None

    @property
    def fraction_used(self) -> float:
        if self.limit is None or self.limit <= 0:
            return 0.0
        return self.used / self.limit


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
    task_center_task_id: str = ""
    tool_call_limit: int | None = None
    tool_calls_used: int = 0
    tool_metadata: ExecutionMetadata | None = None
    enable_background_tasks: bool = False
    terminal_tools: set[str] = field(default_factory=set)
    exit_reason: QueryExitReason | None = None
    terminal_result: ToolResult | None = None
    prompt_report_recorder: PromptReportRecorder | None = None
    # Notification rules evaluated at the top of every turn. See
    # ``notification.rules.dispatch_rules``. Default empty list = disabled.
    notification_rules: list[NotificationRule] = field(default_factory=list)
    # Run-scoped dedup state managed by ``dispatch_rules``: fire_once rule
    # names that have already fired this run.
    notification_fired: set[str] = field(default_factory=set)
    # Free-form per-rule scratchpad keyed by ``rule.name``. Rules own the
    # schema of their own slot (e.g., budget_warning tracks last_fired).
    notification_state: dict[str, Any] = field(default_factory=dict)

    @property
    def tool_budget(self) -> _ToolBudgetView:
        """Read-only view of tool-call budget for notification rule triggers."""
        return _ToolBudgetView(used=self.tool_calls_used, limit=self.tool_call_limit)


def _make_stream_dispatch_deferrer(
    context: QueryContext,
    background_manager: BackgroundTaskManager | None,
) -> Callable[[BaseTool | None, dict[str, Any] | None], bool]:
    """Build a per-stream `should_defer` predicate for `StreamingToolExecutor`.

    Stateful: once a terminal tool is observed,
    every subsequent call returns True for the rest of the stream. Construct
    a fresh predicate for each provider stream.
    """
    exclusive_batch_seen = False

    def _defer(tool_def: BaseTool | None, tool_input: dict[str, Any] | None) -> bool:
        nonlocal exclusive_batch_seen
        if background_manager is not None and defer_background_dispatch(tool_def, tool_input):
            return True
        if exclusive_batch_seen:
            return True
        if tool_def is None:
            return False
        # Terminal tools are batch-exclusive — they must not
        # execute mid-stream alongside siblings. Defer so validate_tool_batch
        # can enforce exclusivity after the full tool_uses list is known.
        is_terminal = tool_def.name in context.terminal_tools
        if is_terminal:
            exclusive_batch_seen = True
            return True
        return False

    return _defer


# ---------------------------------------------------------------------------
# Query loop
# ---------------------------------------------------------------------------


@dataclass
class _StreamRunState:
    """Mutable accumulator for one provider stream."""

    final_message: ConversationMessage | None = None
    usage: UsageSnapshot = field(default_factory=UsageSnapshot)
    streamed_rejections: list[ToolResultBlock] = field(default_factory=list)
    streamed_tool_use_ids: set[str] = field(default_factory=set)
    pending_cancel: dict[str, str] = field(default_factory=dict)


def _initialize_loop_state(
    context: QueryContext,
) -> tuple[BackgroundTaskManager | None, SystemNotificationService]:
    """One-time setup before issuing the provider request."""
    if context.tool_metadata is None:
        context.tool_metadata = ExecutionMetadata()
    elif not isinstance(context.tool_metadata, ExecutionMetadata):
        coerced = ExecutionMetadata()
        coerced.update(context.tool_metadata)
        context.tool_metadata = coerced

    notification_service = ensure_system_notification_service(context.tool_metadata)

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
            if tool.is_terminal_tool
        }

    return background_manager, notification_service


async def _build_stream_executor(
    context: QueryContext,
    background_manager: BackgroundTaskManager | None,
    messages: list[ConversationMessage],
) -> StreamingToolExecutor:
    """Build the streaming tool executor for this provider request."""
    metadata = (
        context.tool_metadata.copy()
        if context.tool_metadata is not None
        else ExecutionMetadata()
    ).with_overrides(conversation_messages=messages)
    if context.task_center_task_id:
        metadata.task_center_task_id = context.task_center_task_id
    execution_context = ToolExecutionContextService(
        cwd=context.cwd,
        services=metadata,
    )
    executor = StreamingToolExecutor(
        tool_registry=context.tool_registry,
        context=execution_context,
        should_defer=_make_stream_dispatch_deferrer(
            context,
            background_manager=background_manager,
        ),
    )
    await prepare_tool_execution_context(context, execution_context)
    return executor


async def _consume_provider_stream(
    context: QueryContext,
    executor: StreamingToolExecutor,
    run_request: QueryRunRequest,
    state: _StreamRunState,
) -> AsyncIterator[tuple[StreamEvent, UsageSnapshot | None]]:
    """Consume the provider stream, populating ``state`` along the way."""
    async for event in context.api_client.stream_message(run_request.request):
        if isinstance(event, ApiThinkingDeltaEvent):
            yield ThinkingDelta(text=event.text), None
            continue

        if isinstance(event, ApiTextDeltaEvent):
            if match := CANCEL_PATTERN.search(event.text):
                tool_id, reason = match.groups()
                state.pending_cancel[tool_id] = reason or "Cancelled by LLM"
            yield AssistantTextDelta(text=event.text), None
            continue

        if isinstance(event, ApiToolUseDeltaEvent):
            state.streamed_tool_use_ids.add(event.id)
            budget_rejection = await _consume_tool_budget_or_reject(
                context,
                event.name,
                event.id,
            )
            if budget_rejection is not None:
                state.streamed_rejections.append(budget_rejection)
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
            state.final_message = event.message
            state.usage = event.usage

    if state.final_message is None:
        raise RuntimeError(
            f"Model stream finished without a final message for model {context.model}. "
            "Check that the API endpoint, authentication, and model name are correct."
        )


async def _drain_executor_after_stream(
    executor: StreamingToolExecutor,
    state: _StreamRunState,
) -> AsyncIterator[tuple[StreamEvent, UsageSnapshot | None]]:
    """Apply LLM-issued cancels and drain final executor events."""
    for tool_id, reason in state.pending_cancel.items():
        executor.cancel(tool_id, reason)
    for progress in executor.get_progress():
        yield progress, None
    for emitted in executor.get_events():
        yield emitted, None


async def _handle_tool_dispatch_branch(
    context: QueryContext,
    messages: list[ConversationMessage],
    executor: StreamingToolExecutor,
    run_request: QueryRunRequest,
    state: _StreamRunState,
    background_manager: BackgroundTaskManager | None,
    notification_service: SystemNotificationService,
) -> AsyncIterator[tuple[StreamEvent, UsageSnapshot | None]]:
    """Dispatch tool calls from the assistant message and append their results."""
    final_message = state.final_message
    assert final_message is not None  # narrowed by _consume_provider_stream

    dispatch = await dispatch_assistant_tools(
        context,
        messages,
        final_message,
        executor,
        streamed_rejections=state.streamed_rejections,
        streamed_tool_use_ids=state.streamed_tool_use_ids,
        background_manager=background_manager,
    )
    for event, event_usage in dispatch.events:
        yield event, event_usage

    tool_results = dispatch.tool_results
    record_tool_results(run_request, tool_results)
    for event in flush_system_notifications(notification_service):
        yield event

    if any_terminal_result(tool_results):
        context.terminal_result = terminal_result_from_tool_results(tool_results)
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
        for event in flush_system_notifications(notification_service):
            yield event
        return

    if tool_results:
        messages.append(ConversationMessage(role="user", content=list(tool_results)))
    context.exit_reason = None


async def _run_query_loop(
    context: QueryContext,
    messages: list[ConversationMessage],
) -> AsyncIterator[tuple[StreamEvent, UsageSnapshot | None]]:
    background_manager, notification_service = _initialize_loop_state(context)

    while True:
        executor = await _build_stream_executor(context, background_manager, messages)

        # Evaluate notification rules and drain any reminders into the
        # transcript before building the next provider request, so newly-
        # fired reminders reach the model on this turn.
        if context.notification_rules:
            await dispatch_rules(
                context.notification_rules,
                messages,
                context,
                notification_service,
                context.notification_fired,
            )
            pending = notification_service.pop_pending_notifications()
            if pending:
                messages.append(
                    ConversationMessage(role="user", content=list(pending))
                )

        state = _StreamRunState()
        run_request = build_query_run_request(context, messages)
        async for event, event_usage in _consume_provider_stream(
            context, executor, run_request, state
        ):
            yield event, event_usage

        async for event, event_usage in _drain_executor_after_stream(executor, state):
            yield event, event_usage

        final_message = state.final_message
        assert final_message is not None  # narrowed by _consume_provider_stream
        messages.append(final_message)
        record_assistant_message(run_request, final_message, state.usage)
        yield AssistantMessageComplete(message=final_message, usage=state.usage), state.usage

        if not final_message.tool_uses:
            for event, event_usage in flush_system_notifications(notification_service):
                yield event, event_usage
            context.exit_reason = QueryExitReason.TEXT_RESPONSE
            break

        async for event, event_usage in _handle_tool_dispatch_branch(
            context,
            messages,
            executor,
            run_request,
            state,
            background_manager,
            notification_service,
        ):
            yield event, event_usage

        if context.exit_reason in {
            QueryExitReason.TOOL_STOP,
            QueryExitReason.RESOURCE_LIMIT,
        }:
            break

    if background_manager is not None and background_manager.has_pending():
        await background_manager.cancel_all()


async def run_query(
    context: QueryContext,
    messages: list[ConversationMessage],
) -> tuple[list[ConversationMessage], AsyncIterator[tuple[StreamEvent, UsageSnapshot | None]]]:
    from dataclasses import fields, is_dataclass, replace

    agent_name = context.agent_name
    run_id = context.run_id

    def _stamp(
        event: StreamEvent,
    ) -> StreamEvent:
        if not is_dataclass(event):
            return event
        if not (agent_name or run_id):
            return event
        names = {f.name for f in fields(event)}
        updates: dict[str, str] = {}
        if "agent_name" in names and not getattr(event, "agent_name", ""):
            updates["agent_name"] = agent_name
        if "run_id" in names and not getattr(event, "run_id", ""):
            updates["run_id"] = run_id
        if not updates:
            return event
        return replace(event, **updates)

    async def _stamped(
        inner: AsyncIterator[tuple[StreamEvent, UsageSnapshot | None]],
    ) -> AsyncIterator[tuple[StreamEvent, UsageSnapshot | None]]:
        async for event, usage in inner:
            yield _stamp(event), usage

    return messages, _stamped(_run_query_loop(context, messages))
