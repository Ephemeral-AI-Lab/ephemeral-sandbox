"""Tool execution logic — handles a single tool call end-to-end."""

from __future__ import annotations

from collections.abc import Awaitable, Callable
from dataclasses import replace
from typing import TYPE_CHECKING, Any

from engine.tool_call.phase_buffer import (
    PHASE_CAPTURE,
    PHASE_EXEC,
    PHASE_QUEUED,
    PHASE_RELEASE,
    record_phase,
)
from message.message import Message
from message.message import ToolResultBlock
from message.events import StreamEvent, ToolExecutionStartedEvent
from sandbox.shared.clock import monotonic_now
from tools._framework.core.base import BaseTool
from tools._framework.core.context import ToolExecutionContextService
from tools._framework.execution.hook_pipeline import ToolHookExecutionPipeline
from tools._framework.core.results import ToolResult
from tools._framework.core.runtime import ExecutionMetadata
from tools._framework.core.validation import execute_tool_body, parse_tool_input, validate_tool_output

if TYPE_CHECKING:
    from engine.api import QueryContext


EmitStreamEvent = Callable[[StreamEvent], Awaitable[None]]


def _count_tool_dispatch(context: QueryContext) -> None:
    """Increment the per-run tool-call counter.

    Soft-limit signaling is delivered via the ``terminal_call_reminder``
    notification rule; hard-failure is the loop's responsibility when
    ``tool_calls_used >= ceil(1.5 * tool_call_limit)``.
    """
    context.tool_calls_used += 1


async def execute_tool_call(
    context: QueryContext,
    tool_name: str,
    tool_use_id: str,
    tool_input: dict[str, object],
    extra_metadata: ExecutionMetadata | dict[str, Any] | None = None,
    conversation_messages: list[Message] | None = None,
) -> ToolResultBlock:
    async def _noop_emit(event: StreamEvent) -> None:
        del event

    return await execute_tool_call_streaming(
        context,
        tool_name,
        tool_use_id,
        tool_input,
        extra_metadata=extra_metadata,
        conversation_messages=conversation_messages,
        emit=_noop_emit,
        emit_started=False,
    )


async def execute_tool_call_streaming(
    context: QueryContext,
    tool_name: str,
    tool_use_id: str,
    tool_input: dict[str, object],
    *,
    emit: "EmitStreamEvent",
    extra_metadata: ExecutionMetadata | dict[str, Any] | None = None,
    conversation_messages: list[Message] | None = None,
    consume_budget: bool = True,
    emit_started: bool = True,
) -> ToolResultBlock:
    """Execute one tool call and emit lifecycle events for the active stream."""
    if consume_budget:
        _count_tool_dispatch(context)

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
    metadata.tool_use_id = tool_use_id
    if context.run_id:
        metadata["query_run_id"] = context.run_id
    if context.task_center_task_id:
        metadata.task_center_task_id = context.task_center_task_id
    if conversation_messages is not None:
        metadata = metadata.with_overrides(conversation_messages=conversation_messages)
    if extra_metadata:
        metadata.update(extra_metadata)

    result = await execute_tool_once(
        tool,
        tool_input,
        ToolExecutionContextService(cwd=context.cwd, services=metadata),
        emit=emit,
        emit_started=emit_started,
    )
    if not result.is_error:
        from tools._framework.execution.trace import record_tool_trace

        record_tool_trace(
            context.tool_metadata,
            tool_name,
            _trace_input_from_result(result, tool_input),
        )

    tool_result = ToolResultBlock(
        tool_use_id=tool_use_id,
        content=result.output,
        is_error=result.is_error,
        metadata=result.metadata,
        is_terminal=result.is_terminal,
    )
    return tool_result


def _trace_input_from_result(
    result: ToolResult,
    fallback: dict[str, object],
) -> dict[str, object]:
    raw = result.metadata.get("effective_tool_input")
    return raw if isinstance(raw, dict) else fallback


async def execute_tool_once(
    tool: BaseTool,
    raw_input: dict[str, Any],
    context: ToolExecutionContextService,
    *,
    emit: EmitStreamEvent,
    emit_started: bool = True,
) -> ToolResult:
    """Validate input, emit start, execute the tool, and validate output.

    Records phase entries (queued/exec/capture/release) into the per-call
    phase buffer set up by the dispatcher. Phases that happen below the
    framework (overlay mount, OCC publish) call :func:`record_phase`
    directly when an active buffer is present — they do not need to be
    aware of this function.
    """
    queued_start = monotonic_now()
    hook_pipeline = ToolHookExecutionPipeline(tool, context, emit)
    parsed = parse_tool_input(tool, raw_input)
    if parsed.error is not None:
        record_phase(PHASE_QUEUED, (monotonic_now() - queued_start) * 1000.0)
        return parsed.error
    assert parsed.args is not None

    parsed_input, hook_failure = await hook_pipeline.run_pre_hooks(parsed.args)
    if hook_failure is not None:
        record_phase(PHASE_QUEUED, (monotonic_now() - queued_start) * 1000.0)
        return hook_failure
    assert parsed_input is not None

    if emit_started:
        await emit(
            ToolExecutionStartedEvent(
                tool_name=tool.name,
                tool_input=parsed_input.model_dump(mode="json"),
                tool_use_id=str(context.tool_use_id or ""),
                agent_name=str(context.agent_name or ""),
                run_id=str(
                    context.get("query_run_id")
                    or context.agent_run_id
                    or context.task_center_task_id
                    or ""
                ),
            )
        )
    record_phase(PHASE_QUEUED, (monotonic_now() - queued_start) * 1000.0)

    exec_start = monotonic_now()
    result = await execute_tool_body(tool, parsed_input, context)
    record_phase(PHASE_EXEC, (monotonic_now() - exec_start) * 1000.0)

    capture_start = monotonic_now()
    validated = validate_tool_output(tool, result)
    hooked = await hook_pipeline.run_post_hooks(parsed_input, validated)
    record_phase(PHASE_CAPTURE, (monotonic_now() - capture_start) * 1000.0)

    release_start = monotonic_now()
    final = hook_pipeline.finalize_result(hooked, effective_input=parsed_input)
    if tool.is_terminal_tool and not final.is_error:
        final = replace(final, is_terminal=True)
    record_phase(PHASE_RELEASE, (monotonic_now() - release_start) * 1000.0)
    return final
