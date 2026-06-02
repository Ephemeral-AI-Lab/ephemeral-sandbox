"""Streaming tool executor for mid-stream tool detection and abort support."""

from __future__ import annotations

import asyncio
import logging
from collections.abc import Callable
from dataclasses import dataclass, field
from enum import StrEnum
from typing import TYPE_CHECKING, Any

from message.events import (
    StreamEvent,
    ToolExecutionCancelledEvent,
    ToolExecutionCompletedEvent,
    ToolExecutionProgressEvent,
)
from engine.background.policy import (
    is_engine_background_tool,
    is_explicit_generic_background_tool,
)
from tools import (
    BaseTool,
    ToolExecutionContextService,
    ToolRegistry,
    ToolResult,
    execute_tool_once,
)

if TYPE_CHECKING:
    from message.events import ToolUseDeltaEvent

logger = logging.getLogger(__name__)


class StreamingToolRunPhase(StrEnum):
    """Internal lifecycle for a streamed foreground tool call."""

    QUEUED = "queued"
    EXECUTING = "executing"
    COMPLETED = "completed"
    YIELDED = "yielded"


@dataclass
class StreamingToolRun:
    tool_use_id: str
    name: str
    input: dict[str, Any]
    phase: StreamingToolRunPhase = StreamingToolRunPhase.QUEUED
    task: asyncio.Task[None] | None = None
    progress_lines: list[str] = field(default_factory=list)
    result: ToolResult | None = None
    cancelled: bool = False
    cancel_reason: str = ""


DeferPredicate = Callable[[BaseTool | None, dict[str, Any] | None], bool]


def defer_background_dispatch(
    tool_def: BaseTool | None, tool_input: dict[str, Any] | None
) -> bool:
    """Default defer predicate for hard-coded engine background tools."""
    return tool_def is not None and (
        is_engine_background_tool(tool_def)
        or is_explicit_generic_background_tool(tool_def, tool_input)
    )


class StreamingToolExecutor:
    """Executes tools as they arrive mid-stream with progress support.

    Features:
    - Tools start executing as soon as tool_use blocks arrive (mid-stream)
    - Progress events stream back for long-running operations
    - Tools the caller flags via ``should_defer`` (e.g. background
      dispatches) are **deferred**: tracked by id but not executed, so
      the query loop can dispatch them through a different path.

    The executor itself has no knowledge of "background" semantics —
    the caller provides ``should_defer`` if it wants deferral. This keeps
    the streaming executor agnostic of engine-level dispatch policy.
    """

    def __init__(
        self,
        tool_registry: ToolRegistry,
        context: ToolExecutionContextService,
        should_defer: DeferPredicate | None = None,
    ):
        self._tool_registry = tool_registry
        self._context = context
        self._should_defer = should_defer
        self._tools: dict[str, StreamingToolRun] = {}
        self._events: list[StreamEvent] = []

    def add_tool(self, event: ToolUseDeltaEvent) -> None:
        """Add a tool to execute as it arrives mid-stream.

        Execution emits ``ToolExecutionStartedEvent`` after input validation, so
        callers should read lifecycle events through :meth:`get_events`.
        """
        tool_def = self._tool_registry.get(event.name)

        # Deferred tools are skipped here — the query loop dispatches them
        # through its background path. The executor doesn't need to track
        # which ids were deferred; dispatch_assistant_tools recovers that by
        # diffing tool_results against the assistant message's tool_uses.
        if self._should_defer is not None and self._should_defer(tool_def, event.input):
            logger.info(
                "STREAM: Deferring tool dispatch: tool_use_id=%s tool_name=%s",
                event.tool_use_id,
                event.name,
            )
            return

        tracked = StreamingToolRun(
            tool_use_id=event.tool_use_id,
            name=event.name,
            input=event.input,
        )
        self._tools[event.tool_use_id] = tracked
        logger.debug(
            "STREAM: Received tool_use event: tool_use_id=%s tool_name=%s input=%s",
            event.tool_use_id,
            event.name,
            event.input,
        )
        if event.input is not None:
            self._start_tool(tracked)
            logger.debug("STREAM: Tool started: tool_use_id=%s tool_name=%s", event.tool_use_id, event.name)
        return

    def get_events(self) -> list[StreamEvent]:
        """Return and clear tool lifecycle events emitted by running tools."""
        events = list(self._events)
        self._events.clear()
        return events

    def get_progress(self) -> list[ToolExecutionProgressEvent]:
        """Get new progress events since last call."""
        events = []
        for tool in self._tools.values():
            if tool.phase == StreamingToolRunPhase.COMPLETED and tool.progress_lines:
                for line in tool.progress_lines:
                    events.append(
                        ToolExecutionProgressEvent(
                            tool_use_id=tool.tool_use_id,
                            tool_name=tool.name,
                            output=line,
                        )
                    )
                tool.progress_lines.clear()
        return events

    async def get_remaining(self) -> list[ToolExecutionCompletedEvent | ToolExecutionCancelledEvent]:
        """Get final results after stream completes.

        Waits for any in-flight tools to finish before returning.
        This prevents the race where MiniMax sends tool_use + complete
        together and the tool hasn't finished executing yet.
        """
        # Wait for in-flight tools to finish
        in_flight = [
            tool.task
            for tool in self._tools.values()
            if tool.phase == StreamingToolRunPhase.EXECUTING and tool.task is not None
        ]
        if in_flight:
            await asyncio.gather(*in_flight, return_exceptions=True)

        results: list[ToolExecutionCompletedEvent | ToolExecutionCancelledEvent] = []
        for tool in self._tools.values():
            if tool.phase == StreamingToolRunPhase.COMPLETED:
                if tool.cancelled:
                    results.append(
                        ToolExecutionCancelledEvent(
                            tool_use_id=tool.tool_use_id,
                            tool_name=tool.name,
                            reason=tool.cancel_reason or "Cancelled by LLM",
                        )
                    )
                elif tool.result:
                    results.append(
                        ToolExecutionCompletedEvent(
                            tool_name=tool.name,
                            output=tool.result.output,
                            is_error=tool.result.is_error,
                            tool_use_id=tool.tool_use_id,
                            metadata=dict(tool.result.metadata or {}),
                            is_terminal=tool.result.is_terminal,
                        )
                    )
                tool.phase = StreamingToolRunPhase.YIELDED
        return results

    def _start_tool(self, tool: StreamingToolRun) -> None:
        """Start executing a tool."""
        tool.phase = StreamingToolRunPhase.EXECUTING
        tool.task = asyncio.create_task(self._execute_tool(tool))

    async def _execute_tool(self, tool: StreamingToolRun) -> None:
        """Execute a single tool with progress tracking."""
        logger.debug("STREAM: Executing tool: tool_use_id=%s tool_name=%s", tool.tool_use_id, tool.name)
        try:
            tool_def = self._tool_registry.get(tool.name)
            if not tool_def:
                logger.warning("STREAM: Unknown tool: tool_use_id=%s tool_name=%s", tool.tool_use_id, tool.name)
                tool.result = ToolResult(
                    output=f"Unknown tool: {tool.name}",
                    is_error=True,
                )
                tool.phase = StreamingToolRunPhase.COMPLETED
                return

            context_with_id = ToolExecutionContextService(
                cwd=self._context.cwd,
                services=self._context.services_with_overrides(tool_use_id=tool.tool_use_id),
            )

            tool.result = await execute_tool_once(
                tool_def,
                tool.input,
                context_with_id,
                emit=self._emit_event,
            )
            logger.debug(
                "STREAM: Tool completed: tool_use_id=%s tool_name=%s is_error=%s output_len=%d",
                tool.tool_use_id,
                tool.name,
                tool.result.is_error,
                len(tool.result.output) if tool.result.output else 0,
            )
        except asyncio.CancelledError:
            logger.info("STREAM: Tool cancelled during execution: tool_use_id=%s", tool.tool_use_id)
            tool.cancelled = True
            tool.cancel_reason = tool.cancel_reason or "Task cancelled"
        finally:
            tool.phase = StreamingToolRunPhase.COMPLETED

    def cancel_all(self) -> None:
        """Cancel all running tasks to prevent orphaned execution."""
        for tool in self._tools.values():
            if tool.task and not tool.task.done():
                tool.task.cancel()
                tool.cancelled = True
                tool.cancel_reason = "Superseded by fallback execution"

    async def _emit_event(self, event: StreamEvent) -> None:
        self._events.append(event)
