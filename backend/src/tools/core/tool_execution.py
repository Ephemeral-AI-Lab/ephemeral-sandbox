"""Tool execution logic — handles a single tool call end-to-end."""

from __future__ import annotations

from collections.abc import Awaitable, Callable
from dataclasses import replace
from typing import TYPE_CHECKING, Any

from message.messages import ToolResultBlock
from message.stream_events import StreamEvent, ToolExecutionStarted
from tools.core.base import (
    BaseTool,
    ExecutionMetadata,
    ToolExecutionContextService,
    ToolResult,
    execute_tool_body,
    parse_tool_input,
    validate_tool_output,
)

if TYPE_CHECKING:
    from agents.types import ModeDefinition
    from engine.core.query import QueryContext


EmitStreamEvent = Callable[[StreamEvent], Awaitable[None]]


def _build_budget_exceeded_error(
    tool_use_id: str,
    tool_call_limit: int,
) -> ToolResultBlock:
    return ToolResultBlock(
        tool_use_id=tool_use_id,
        content=(
            f"tool_call_limit exceeded: {tool_call_limit} tool "
            f"calls already used. The agent run will terminate after "
            f"this turn — wrap up and summarize your progress now to "
            f"preserve partial work."
        ),
        is_error=True,
    )


def _build_terminal_budget_reserved_error(
    tool_use_id: str,
    tool_call_limit: int,
    terminal_tools: set[str],
) -> ToolResultBlock:
    tool_list = ", ".join(sorted(terminal_tools))
    return ToolResultBlock(
        tool_use_id=tool_use_id,
        content=(
            f"tool_call_limit terminal call reserved: {tool_call_limit - 1} "
            f"of {tool_call_limit} tool calls already used. The last call is "
            f"reserved for terminal submission via {tool_list}."
        ),
        is_error=True,
    )


def _build_mode_deny(
    tool_name: str,
    tool_use_id: str,
    mode: ModeDefinition,
) -> ToolResultBlock:
    terminals = ", ".join(sorted(mode.terminals)) or "(none)"
    return ToolResultBlock(
        tool_use_id=tool_use_id,
        content=(
            f"`{tool_name}` not allowed in `{mode.name}` mode. "
            f"Allowed terminals: {terminals}. "
            "Use read/search/explore tools or call a terminal."
        ),
        is_error=True,
    )


def evaluate_mode_gate(
    active_mode: "ModeDefinition | None",
    tool_name: str,
    tool_use_id: str,
) -> ToolResultBlock | None:
    """Decide whether *tool_name* may run under *active_mode*.

    Returns ``None`` to allow, or a structured deny ``ToolResultBlock`` whose
    body matches the format described in
    ``docs/architecture/agent-mode-system-v1.md`` §Authorization gate.

    Decision order:
        - active_mode is None         → allow (gating disabled)
        - tool in terminals           → allow
        - tool in allowed_tools       → allow
        - else                        → deny
    """
    if active_mode is None:
        return None
    if tool_name in active_mode.terminals:
        return None
    if tool_name in active_mode.allowed_tools:
        return None
    return _build_mode_deny(tool_name, tool_use_id, active_mode)


def _consume_tool_budget_or_reject(
    context: QueryContext,
    tool_name: str,
    tool_use_id: str,
) -> ToolResultBlock | None:
    if context.tool_call_limit is None:
        return None
    if context.tool_calls_used >= context.tool_call_limit:
        if tool_name in context.terminal_tools:
            return None
        return _build_budget_exceeded_error(tool_use_id, context.tool_call_limit)
    if (
        context.terminal_tools
        and context.tool_calls_used == context.tool_call_limit - 1
        and tool_name not in context.terminal_tools
    ):
        return _build_terminal_budget_reserved_error(
            tool_use_id,
            context.tool_call_limit,
            context.terminal_tools,
        )
    context.tool_calls_used += 1
    return None


async def execute_tool_call(
    context: QueryContext,
    tool_name: str,
    tool_use_id: str,
    tool_input: dict[str, object],
    extra_metadata: ExecutionMetadata | dict[str, Any] | None = None,
) -> ToolResultBlock:
    async def _noop_emit(event: StreamEvent) -> None:
        del event

    return await execute_tool_call_streaming(
        context,
        tool_name,
        tool_use_id,
        tool_input,
        extra_metadata=extra_metadata,
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
    consume_budget: bool = True,
    emit_started: bool = True,
) -> ToolResultBlock:
    """Execute one tool call and emit lifecycle events for the active stream."""
    # Mode gate runs before budget consumption so denied calls never burn
    # the agent's tool-call quota — see docs/architecture/agent-mode-system-v1.md.
    mode_rejection = evaluate_mode_gate(context.active_mode, tool_name, tool_use_id)
    if mode_rejection is not None:
        return mode_rejection
    if consume_budget:
        budget_rejection = _consume_tool_budget_or_reject(context, tool_name, tool_use_id)
        if budget_rejection is not None:
            return budget_rejection

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
    metadata.tool_id = tool_use_id
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
        from engine.runtime.tool_trace import record_tool_trace

        record_tool_trace(
            context.tool_metadata,
            tool_name,
            tool_input,
            tool_use_id=tool_use_id,
        )

    tool_result = ToolResultBlock(
        tool_use_id=tool_use_id,
        content=result.output,
        is_error=result.is_error,
        metadata=result.metadata,
        does_terminate=result.does_terminate,
        mode_transition=result.mode_transition,
    )
    return tool_result


async def execute_tool_once(
    tool: BaseTool,
    raw_input: dict[str, Any],
    context: ToolExecutionContextService,
    *,
    emit: EmitStreamEvent,
    emit_started: bool = True,
) -> ToolResult:
    """Validate input, emit start, execute the tool, and validate output."""
    parsed = parse_tool_input(tool, raw_input)
    if parsed.error is not None:
        return parsed.error
    assert parsed.args is not None

    if emit_started:
        await emit(
            ToolExecutionStarted(
                tool_name=tool.name,
                tool_input=parsed.args.model_dump(mode="json"),
            )
        )

    result = await execute_tool_body(tool, parsed.args, context)
    validated = validate_tool_output(tool, result)
    if tool.is_terminal_tool and not validated.is_error:
        return replace(validated, does_terminate=True)
    return validated
