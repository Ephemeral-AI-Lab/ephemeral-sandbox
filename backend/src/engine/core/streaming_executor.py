"""Streaming tool executor for mid-stream tool detection and abort support."""

from __future__ import annotations

import asyncio
import logging
from collections.abc import Callable
from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Any

from message.stream_events import (
    StreamEvent,
    ToolExecutionCancelled,
    ToolExecutionCompleted,
    ToolExecutionProgress,
)
from tools import (
    BaseTool,
    ToolExecutionContextService,
    ToolRegistry,
    ToolResult,
    execute_tool_once,
)

if TYPE_CHECKING:
    from providers.types import ApiToolUseDeltaEvent

logger = logging.getLogger(__name__)


@dataclass
class TrackedTool:
    id: str
    name: str
    input: dict[str, Any]
    status: str = "queued"
    task: asyncio.Task | None = None
    progress_lines: list[str] = field(default_factory=list)
    result: ToolResult | None = None
    cancelled: bool = False
    cancel_reason: str = ""


DeferPredicate = Callable[[BaseTool | None, dict[str, Any] | None], bool]


def defer_background_dispatch(
    tool_def: BaseTool | None, tool_input: dict[str, Any] | None
) -> bool:
    """Default defer predicate: skip tools that should run in the background.

    A tool is deferred when it has ``background="always"`` or when it
    has ``background="optional"`` and the LLM explicitly requested
    background execution via the input flag. Callers wire this into
    :class:`StreamingToolExecutor` via ``should_defer`` so the executor
    itself never inspects the ``background`` attribute directly.
    """
    if tool_def is None:
        return False
    bg_mode = getattr(tool_def, "background", "forbidden")
    if bg_mode == "always":
        return True
    if bg_mode == "optional" and tool_input and tool_input.get("background"):
        return True
    return False


class StreamingToolExecutor:
    """Executes tools as they arrive mid-stream with progress support.

    Features:
    - Tools start executing as soon as tool_use blocks arrive (mid-stream)
    - Progress events stream back for long-running operations
    - LLM can abort tools via cancel() signal
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
        self._tools: dict[str, TrackedTool] = {}
        self._aborted: set[str] = set()
        self._deferred: set[str] = set()
        self._events: list[StreamEvent] = []

    @property
    def deferred_dispatch_ids(self) -> set[str]:
        """IDs of tool_uses the caller asked us to defer (not execute)."""
        return self._deferred

    @property
    def context(self) -> ToolExecutionContextService:
        """Tool execution context used by this executor."""
        return self._context

    def add_tool(self, event: ApiToolUseDeltaEvent) -> None:
        """Add a tool to execute as it arrives mid-stream.

        Execution emits ``ToolExecutionStarted`` after input validation, so
        callers should read lifecycle events through :meth:`get_events`.
        """
        tool_def = self._tool_registry.get(event.name)

        # Deferred tools are tracked by id but never executed here —
        # the query loop dispatches them through its background path.
        if self._should_defer is not None and self._should_defer(tool_def, event.input):
            self._deferred.add(event.id)
            logger.info(
                "STREAM: Deferring tool dispatch: tool_id=%s tool_name=%s",
                event.id,
                event.name,
            )
            return

        tracked = TrackedTool(
            id=event.id,
            name=event.name,
            input=event.input,
        )
        self._tools[event.id] = tracked
        logger.debug(
            "STREAM: Received tool_use event: tool_id=%s tool_name=%s input=%s",
            event.id,
            event.name,
            event.input,
        )
        if event.input is not None:
            self._start_tool(tracked)
            logger.debug("STREAM: Tool started: tool_id=%s tool_name=%s", event.id, event.name)
        return

    def get_events(self) -> list[StreamEvent]:
        """Return and clear tool lifecycle events emitted by running tools."""
        events = list(self._events)
        self._events.clear()
        return events

    def cancel(self, tool_id: str, reason: str) -> None:
        """Cancel a running tool."""
        logger.info("STREAM: Cancel requested: tool_id=%s reason=%s", tool_id, reason)
        self._aborted.add(tool_id)
        if tool_id in self._tools:
            self._tools[tool_id].cancelled = True
            self._tools[tool_id].cancel_reason = reason
            task = self._tools[tool_id].task
            if task and not task.done():
                task.cancel()
                logger.info("STREAM: Cancel signal sent: tool_id=%s", tool_id)

    def get_progress(self) -> list[ToolExecutionProgress]:
        """Get new progress events since last call."""
        events = []
        for tool in self._tools.values():
            if tool.status == "completed" and tool.progress_lines:
                for line in tool.progress_lines:
                    events.append(
                        ToolExecutionProgress(
                            tool_id=tool.id,
                            tool_name=tool.name,
                            output=line,
                        )
                    )
                tool.progress_lines.clear()
        return events

    async def get_remaining(self) -> list[ToolExecutionCompleted | ToolExecutionCancelled]:
        """Get final results after stream completes.

        Waits for any in-flight tools to finish before returning.
        This prevents the race where MiniMax sends tool_use + complete
        together and the tool hasn't finished executing yet.
        """
        # Wait for in-flight tools to finish
        in_flight = [
            tool.task
            for tool in self._tools.values()
            if tool.status == "executing" and tool.task is not None
        ]
        if in_flight:
            await asyncio.gather(*in_flight, return_exceptions=True)

        results = []
        for tool in self._tools.values():
            if tool.status == "completed":
                if tool.cancelled:
                    results.append(
                        ToolExecutionCancelled(
                            tool_id=tool.id,
                            tool_name=tool.name,
                            reason=tool.cancel_reason or "Cancelled by LLM",
                        )
                    )
                elif tool.result:
                    results.append(
                        ToolExecutionCompleted(
                            tool_name=tool.name,
                            output=tool.result.output,
                            is_error=tool.result.is_error,
                            tool_id=tool.id,
                            metadata=dict(tool.result.metadata or {}),
                            does_terminate=tool.result.does_terminate,
                        )
                    )
                tool.status = "yielded"
        return results

    def _start_tool(self, tool: TrackedTool) -> None:
        """Start executing a tool."""
        tool.status = "executing"
        tool.task = asyncio.create_task(self._execute_tool(tool))

    async def _execute_tool(self, tool: TrackedTool) -> None:
        """Execute a single tool with progress tracking."""
        logger.debug("STREAM: Executing tool: tool_id=%s tool_name=%s", tool.id, tool.name)
        try:
            if tool.id in self._aborted:
                logger.info("STREAM: Tool aborted before execution: tool_id=%s", tool.id)
                tool.status = "completed"
                tool.cancelled = True
                return

            tool_def = self._tool_registry.get(tool.name)
            if not tool_def:
                logger.warning("STREAM: Unknown tool: tool_id=%s tool_name=%s", tool.id, tool.name)
                tool.result = ToolResult(
                    output=f"Unknown tool: {tool.name}",
                    is_error=True,
                )
                tool.status = "completed"
                return

            context_with_id = ToolExecutionContextService(
                cwd=self._context.cwd,
                services=self._context.services_with_overrides(tool_id=tool.id),
            )

            tool.result = await execute_tool_once(
                tool_def,
                tool.input,
                context_with_id,
                emit=self._emit_event,
            )
            logger.debug(
                "STREAM: Tool completed: tool_id=%s tool_name=%s is_error=%s output_len=%d",
                tool.id,
                tool.name,
                tool.result.is_error,
                len(tool.result.output) if tool.result.output else 0,
            )
        except asyncio.CancelledError:
            logger.info("STREAM: Tool cancelled during execution: tool_id=%s", tool.id)
            tool.cancelled = True
            tool.cancel_reason = tool.cancel_reason or "Task cancelled"
        finally:
            tool.status = "completed"

    def cancel_all(self) -> None:
        """Cancel all running tasks to prevent orphaned execution."""
        for tool in self._tools.values():
            if tool.task and not tool.task.done():
                tool.task.cancel()
                tool.cancelled = True
                tool.cancel_reason = "Superseded by fallback execution"

    async def _emit_event(self, event: StreamEvent) -> None:
        self._events.append(event)
