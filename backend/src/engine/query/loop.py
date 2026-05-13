"""Core tool-aware query loop."""

from __future__ import annotations

import logging
from collections.abc import AsyncIterator, Callable, Mapping
from dataclasses import dataclass, field
from typing import Any

from providers.types import (
    ApiCancelEvent,
    ApiMessageCompleteEvent,
    ApiTextDeltaEvent,
    ApiThinkingDeltaEvent,
    ApiToolUseDeltaEvent,
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
from engine.query.notifications import (
    ensure_system_notification_service,
    flush_system_notifications,
)
from engine.tool_call.streaming import StreamingToolExecutor, defer_background_dispatch
from engine.tool_call.dispatch import dispatch_assistant_tools
from engine.tool_call.result import (
    any_terminal_result,
    terminal_result_from_tool_results,
)
from engine.query.request import (
    QueryRunRequest,
    build_query_run_request,
    record_assistant_message,
    record_tool_results,
)
from engine.query.context import QueryContext, QueryExitReason
from engine.background.manager import BackgroundTaskManager
from engine.tool_call.context import prepare_tool_execution_context
from notification import dispatch_rules
from notification import SystemNotificationService
from tools import (
    BaseTool,
    ExecutionMetadata,
    ToolExecutionContextService,
    _consume_tool_budget_or_reject,
)


logger = logging.getLogger(__name__)

def _make_stream_dispatch_deferrer(
    context: QueryContext,
    background_manager: BackgroundTaskManager | None,
) -> Callable[[BaseTool | None, dict[str, Any] | None], bool]:
    """Build a per-stream `should_defer` predicate for `StreamingToolExecutor`.

    Terminal-capable runs defer every tool until the complete assistant message
    is available, so terminal-tool exclusivity is validated before any sibling
    tool body can run.
    """

    def _defer(tool_def: BaseTool | None, tool_input: dict[str, Any] | None) -> bool:
        if background_manager is not None and defer_background_dispatch(tool_def, tool_input):
            return True
        if context.terminal_tools:
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


def _initialize_loop_state(
    context: QueryContext,
) -> tuple[BackgroundTaskManager | None, SystemNotificationService]:
    """One-time setup before issuing the provider request."""
    if context.tool_metadata is None:
        context.tool_metadata = ExecutionMetadata()
    elif not isinstance(context.tool_metadata, ExecutionMetadata):
        if not isinstance(context.tool_metadata, Mapping):
            raise TypeError(
                "tool_metadata must be ExecutionMetadata or Mapping, "
                f"got {type(context.tool_metadata).__name__}"
            )
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
    try:
        async for event in context.api_client.stream_message(run_request.request):
            if isinstance(event, ApiThinkingDeltaEvent):
                yield ThinkingDelta(text=event.text), None
                continue

            if isinstance(event, ApiTextDeltaEvent):
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
                "This may indicate a provider error, content-filter cutoff, or "
                "misconfigured API endpoint / authentication / model name."
            )
    except BaseException:
        executor.cancel_all()
        raise


async def _drain_executor_after_stream(
    executor: StreamingToolExecutor,
) -> AsyncIterator[tuple[StreamEvent, UsageSnapshot | None]]:
    """Drain final executor progress and lifecycle events."""
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

    try:
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

            async for event, event_usage in _drain_executor_after_stream(executor):
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
    finally:
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
