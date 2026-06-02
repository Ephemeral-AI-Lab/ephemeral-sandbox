"""Background tool dispatch plumbing used by the query loop."""

from __future__ import annotations

from collections.abc import Awaitable, Callable
from typing import TYPE_CHECKING, cast
from uuid import uuid4

from pydantic import ValidationError

from engine.background.policy import (
    BACKGROUND_TOOL_INPUT_KEY,
    is_engine_background_tool,
    is_explicit_generic_background_tool,
)
from engine.background.task_supervisor import SUBAGENT_TASK_TYPE, BackgroundTaskSupervisor
from message.message import Message, ToolResultBlock, ToolUseBlock
from message.events import (
    BackgroundTaskStartedEvent,
    StreamEvent,
    ToolExecutionCompletedEvent,
)
from notification import SystemNotification
from tools import (
    BaseTool,
    ExecutionMetadata,
    SANDBOX_CONTEXT,
    ToolRegistry,
    ToolResult,
)
from tools._framework.execution.trace import record_tool_trace

if TYPE_CHECKING:
    from engine.query.context import QueryContext

ToolCallExecutor = Callable[
    [str, str, dict[str, object], ExecutionMetadata | None],
    Awaitable[ToolResultBlock],
]
SANDBOX_INVOCATION_ID_INPUT_KEY = "_sandbox_invocation_id"
DISABLE_SANDBOX_HEARTBEAT_INPUT_KEY = "_disable_sandbox_heartbeat"
_BACKGROUND_CONTROL_INPUT_KEYS = frozenset(
    {
        BACKGROUND_TOOL_INPUT_KEY,
        SANDBOX_INVOCATION_ID_INPUT_KEY,
        DISABLE_SANDBOX_HEARTBEAT_INPUT_KEY,
    }
)


def validate_background_tool_input(
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
    background_tasks: BackgroundTaskSupervisor,
    tool_use: ToolUseBlock,
    execute_tool_call: ToolCallExecutor,
) -> tuple[ToolResultBlock, BackgroundTaskStartedEvent | None, ToolExecutionCompletedEvent | None]:
    """Dispatch a single tool use through the background path."""
    clean_input = {
        k: v for k, v in tool_use.input.items() if k not in _BACKGROUND_CONTROL_INPUT_KEYS
    }

    tool_def = tool_registry.get(tool_use.name)
    explicit_generic = is_explicit_generic_background_tool(tool_def, tool_use.input)
    if tool_def is None or not (is_engine_background_tool(tool_def) or explicit_generic):
        msg = f"Tool '{tool_use.name}' does not support background execution."
        return (
            ToolResultBlock(tool_use_id=tool_use.tool_use_id, content=msg, is_error=True),
            None,
            ToolExecutionCompletedEvent(tool_name=tool_use.name, output=msg, is_error=True),
        )

    validation_result = validate_background_tool_input(
        tool_def=tool_def,
        clean_input=clean_input,
    )
    if validation_result is not None:
        return (
            ToolResultBlock(
                tool_use_id=tool_use.tool_use_id,
                content=validation_result.output,
                is_error=validation_result.is_error,
                metadata=validation_result.metadata,
            ),
            None,
            ToolExecutionCompletedEvent(
                tool_name=tool_use.name,
                output=validation_result.output,
                is_error=validation_result.is_error,
                tool_use_id=tool_use.tool_use_id,
                metadata=dict(validation_result.metadata or {}),
            ),
        )

    task_type = getattr(tool_def, "task_type", "agent")
    if task_type != SUBAGENT_TASK_TYPE and not explicit_generic:
        msg = f"Tool '{tool_use.name}' does not support background-manager dispatch."
        return (
            ToolResultBlock(tool_use_id=tool_use.tool_use_id, content=msg, is_error=True),
            None,
            ToolExecutionCompletedEvent(tool_name=tool_use.name, output=msg, is_error=True),
        )
    task_id = (
        background_tasks.next_subagent_session_id()
        if task_type == SUBAGENT_TASK_TYPE
        else background_tasks.next_alias()
    )
    uses_sandbox = SANDBOX_CONTEXT in getattr(
        tool_def,
        "context_requirements",
        (),
    )
    sandbox_id = str(getattr(tool_metadata, "sandbox_id", "") or "")
    requested_sandbox_invocation_id = str(
        tool_use.input.get(SANDBOX_INVOCATION_ID_INPUT_KEY) or ""
    ).strip()
    sandbox_invocation_id = ""
    if uses_sandbox and sandbox_id:
        sandbox_invocation_id = requested_sandbox_invocation_id or uuid4().hex
    heartbeat_enabled = not (
        bool(requested_sandbox_invocation_id)
        and bool(tool_use.input.get(DISABLE_SANDBOX_HEARTBEAT_INPUT_KEY))
    )
    agent_id = str(getattr(tool_metadata, "agent_run_id", "") or "")
    if not agent_id:
        agent_id = str(getattr(tool_metadata, "agent_name", "") or "")

    async def _run_background_tool(alias: str = task_id) -> ToolResult:
        background_metadata = ExecutionMetadata(
            on_progress_line=background_tasks.make_progress_callback(alias),
            background_task_id=alias,
            sandbox_invocation_id=sandbox_invocation_id or None,
        )
        block = await execute_tool_call(
            tool_use.name,
            tool_use.tool_use_id,
            clean_input,
            background_metadata,
        )
        return ToolResult(
            output=block.content,
            is_error=block.is_error,
            metadata=dict(block.metadata or {}),
        )

    started_event = background_tasks.launch(
        task_id,
        tool_use.name,
        clean_input,
        _run_background_tool(),
        task_type=task_type,
        subagent_session_id=task_id if task_type == SUBAGENT_TASK_TYPE else None,
        agent_id=agent_id or None,
        uses_sandbox=uses_sandbox,
        sandbox_id=sandbox_id or None,
        sandbox_invocation_id=sandbox_invocation_id or None,
        heartbeat_enabled=heartbeat_enabled,
    )
    record_tool_trace(tool_metadata, tool_use.name, clean_input)
    if task_type == SUBAGENT_TASK_TYPE:
        content = (
            f'[SUBAGENT LAUNCHED] subagent_session_id="{task_id}" '
            f'status=running agent_name="{clean_input.get("agent_name", "")}"\n'
            f"Use check_subagent_progress("
            f'subagent_session_id="{task_id}", last_n_messages=5) '
            f"to inspect progress, or cancel_subagent("
            f'subagent_session_id="{task_id}") to stop it. '
            f"Keep using the current response on other ready work first."
        )
    else:
        content = (
            f'[BACKGROUND LAUNCHED] task_id="{task_id}" '
            f'status=running tool_name="{tool_use.name}"\n'
            f'Use check_background_task_result(task_id="{task_id}") to inspect '
            f'progress, or cancel_background_task(task_id="{task_id}") to stop it.'
        )
    tool_result = ToolResultBlock(
        tool_use_id=tool_use.tool_use_id,
        content=content,
        is_error=False,
    )
    return tool_result, started_event, None


def dispatch_background_tool_call(
    context: QueryContext,
    conversation_messages: list[Message],
    background_tasks: BackgroundTaskSupervisor,
    tool_call: ToolUseBlock,
    tool_results: list[ToolResultBlock],
) -> list[StreamEvent]:
    async def _execute_background_tool_call(
        tool_name: str,
        tool_use_id: str,
        tool_input: dict[str, object],
        extra_metadata: ExecutionMetadata | None = None,
    ) -> ToolResultBlock:
        from tools import execute_tool_call_streaming

        async def emit(event: StreamEvent) -> None:
            if not isinstance(event, SystemNotification):
                return
            callback = (
                extra_metadata.on_progress_line
                if isinstance(extra_metadata, ExecutionMetadata)
                else None
            )
            if callback is not None:
                callback(event.text)

        return cast(
            ToolResultBlock,
            await execute_tool_call_streaming(
                context,
                tool_name,
                tool_use_id,
                tool_input,
                emit=emit,
                extra_metadata=extra_metadata,
                conversation_messages=conversation_messages,
            ),
        )

    tool_result, started_event, rejection_event = launch_background_tool(
        tool_registry=context.tool_registry,
        tool_metadata=context.tool_metadata,
        background_tasks=background_tasks,
        tool_use=tool_call,
        execute_tool_call=_execute_background_tool_call,
    )
    tool_results.append(tool_result)
    events: list[StreamEvent] = []
    if started_event is not None:
        events.append(started_event)
    if rejection_event is not None:
        events.append(rejection_event)
    return events
