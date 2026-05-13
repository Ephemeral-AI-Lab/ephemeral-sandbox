"""Background tool dispatch plumbing used by the query loop."""

from __future__ import annotations

from collections.abc import Awaitable, Callable
from typing import TYPE_CHECKING, Any

from pydantic import ValidationError

from engine.background.manager import BackgroundTaskManager
from engine.tool_call.trace import record_tool_trace
from message.messages import ConversationMessage, ToolResultBlock, ToolUseBlock
from message.stream_events import (
    BackgroundTaskStarted,
    StreamEvent,
    ToolExecutionCompleted,
)
from notification import SystemNotification
from providers.types import UsageSnapshot
from tools import BaseTool, ExecutionMetadata, ToolRegistry, ToolResult

if TYPE_CHECKING:
    from engine.query.context import QueryContext

ToolCallExecutor = Callable[
    [str, str, dict[str, object], ExecutionMetadata | dict[str, Any] | None],
    Awaitable[ToolResultBlock],
]


def validate_background_input(
    tool_def: BaseTool,
    clean_input: dict[str, object],
) -> ToolResult | None:
    """Validate a background launch before spawning the async task."""
    try:
        tool_def.input_model.model_validate(clean_input)
    except ValidationError as exc:
        errors = "; ".join(
            f"{'.'.join(str(p) for p in e['loc'])}: {e['msg']}" for e in exc.errors()
        )
        return ToolResult(
            output=(
                f"Invalid input for {tool_def.name}: {errors}. "
                "Please retry the tool call with valid arguments."
            ),
            is_error=True,
        )
    except Exception as exc:
        return ToolResult(
            output=f"Invalid input for {tool_def.name}: {exc}",
            is_error=True,
        )
    return None


def launch_background_tool(
    *,
    tool_registry: ToolRegistry,
    tool_metadata: ExecutionMetadata | None,
    background_manager: BackgroundTaskManager,
    tool_use: ToolUseBlock,
    execute_tool_call: ToolCallExecutor,
) -> tuple[ToolResultBlock, BackgroundTaskStarted | None, ToolExecutionCompleted | None]:
    """Dispatch a single tool use through the background path."""
    clean_input = {k: v for k, v in tool_use.input.items() if k != "background"}

    tool_def = tool_registry.get(tool_use.name)
    if tool_def is None or getattr(tool_def, "background", "forbidden") == "forbidden":
        msg = f"Tool '{tool_use.name}' does not support background execution."
        return (
            ToolResultBlock(tool_use_id=tool_use.id, content=msg, is_error=True),
            None,
            ToolExecutionCompleted(tool_name=tool_use.name, output=msg, is_error=True),
        )

    # TODO(engine/CR-01): kill_callback is intentionally None because the
    # sandbox package does not yet expose a per-process kill primitive.
    # BackgroundTaskManager.cancel/cancel_all therefore fall through to
    # asyncio.Task.cancel() for non-subagent tools, which (per
    # manager.cancel's docstring) can corrupt the shared sandbox connection
    # when the task is in flight inside a sandbox exec. Pure-Python tools
    # are safe; sandbox-backed tools (e.g. shell with background="optional")
    # are not. Wire a real kill_callback once sandbox.api exposes one.
    kill_callback = None
    validation_result = validate_background_input(
        tool_def=tool_def,
        clean_input=clean_input,
    )
    if validation_result is not None:
        return (
            ToolResultBlock(
                tool_use_id=tool_use.id,
                content=validation_result.output,
                is_error=validation_result.is_error,
                metadata=validation_result.metadata,
            ),
            None,
            ToolExecutionCompleted(
                tool_name=tool_use.name,
                output=validation_result.output,
                is_error=validation_result.is_error,
                tool_id=tool_use.id,
                metadata=dict(validation_result.metadata or {}),
            ),
        )

    bg_alias = background_manager.next_alias()

    async def _bg_wrapper(alias: str = bg_alias) -> ToolResult:
        bg_overrides = ExecutionMetadata(
            on_progress_line=background_manager.make_progress_callback(alias),
            background_task_id=alias,
        )
        block = await execute_tool_call(
            tool_use.name,
            tool_use.id,
            clean_input,
            bg_overrides,
        )
        return ToolResult(
            output=block.content,
            is_error=block.is_error,
            metadata=dict(block.metadata or {}),
        )

    bg_event = background_manager.launch(
        bg_alias,
        tool_use.name,
        clean_input,
        _bg_wrapper(),
        kill_callback=kill_callback,
        task_type=getattr(tool_def, "task_type", "agent"),
    )
    record_tool_trace(tool_metadata, tool_use.name, clean_input)
    tool_result = ToolResultBlock(
        tool_use_id=tool_use.id,
        content=(
            f'[BACKGROUND LAUNCHED] task_id="{bg_alias}" tool={tool_use.name}\n'
            f"Use this task_id with "
            f'check_background_task_result(task_id="{bg_alias}"), '
            f"wait_background_tasks() to block until all tasks settle, or "
            f'cancel_background_task(task_id="{bg_alias}"). '
            f"Keep using the current response on other ready work first; do not "
            f"wait immediately unless this task is the only blocker left."
        ),
        is_error=False,
    )
    return tool_result, bg_event, None


def launch_and_collect_bg_events(
    context: QueryContext,
    conversation_messages: list[ConversationMessage],
    background_manager: BackgroundTaskManager,
    tc: ToolUseBlock,
    tool_results: list[ToolResultBlock],
) -> list[tuple[StreamEvent, UsageSnapshot | None]]:
    async def _execute_in_context(
        tool_name: str,
        tool_use_id: str,
        tool_input: dict[str, object],
        extra_metadata: ExecutionMetadata | dict[str, Any] | None = None,
    ) -> ToolResultBlock:
        from tools import execute_tool_call_streaming

        async def emit(event: StreamEvent) -> None:
            if not isinstance(event, SystemNotification):
                return
            callback = None
            if isinstance(extra_metadata, ExecutionMetadata):
                callback = extra_metadata.on_progress_line
            elif isinstance(extra_metadata, dict):
                raw = extra_metadata.get("on_progress_line")
                callback = raw if callable(raw) else None
            if callback is not None:
                callback(event.text)

        return await execute_tool_call_streaming(
            context,
            tool_name,
            tool_use_id,
            tool_input,
            emit=emit,
            extra_metadata=extra_metadata,
            conversation_messages=conversation_messages,
        )

    tool_result, bg_event, reject_event = launch_background_tool(
        tool_registry=context.tool_registry,
        tool_metadata=context.tool_metadata,
        background_manager=background_manager,
        tool_use=tc,
        execute_tool_call=_execute_in_context,
    )
    tool_results.append(tool_result)
    events: list[tuple[StreamEvent, UsageSnapshot | None]] = []
    if bg_event is not None:
        events.append((bg_event, None))
    if reject_event is not None:
        events.append((reject_event, None))
    return events
