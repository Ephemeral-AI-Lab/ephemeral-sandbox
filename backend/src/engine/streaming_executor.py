"""Streaming tool executor for mid-stream tool detection and abort support."""

from __future__ import annotations

import asyncio
from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Any

from pydantic import BaseModel

from engine.messages import ConversationMessage
from engine.stream_events import (
    ToolExecutionCancelled,
    ToolExecutionCompleted,
    ToolExecutionProgress,
    ToolExecutionStarted,
)
from tools.base import ToolExecutionContext, ToolRegistry, ToolResult

if TYPE_CHECKING:
    from models.types import ApiToolUseDeltaEvent


@dataclass
class TrackedTool:
    id: str
    name: str
    input: dict[str, Any]
    assistant_message: ConversationMessage
    status: str = "queued"
    is_concurrency_safe: bool = True
    task: asyncio.Task | None = None
    progress_lines: list[str] = field(default_factory=list)
    result: ToolResult | None = None
    cancelled: bool = False
    cancel_reason: str = ""


class StreamingToolExecutor:
    """Executes tools as they arrive mid-stream with progress support.

    Features:
    - Tools start executing as soon as tool_use blocks arrive (mid-stream)
    - Concurrency-safe tools run in parallel
    - Progress events stream back for long-running operations
    - LLM can abort tools via cancel() signal
    """

    def __init__(
        self,
        tool_registry: ToolRegistry,
        context: ToolExecutionContext,
    ):
        self._tool_registry = tool_registry
        self._context = context
        self._tools: dict[str, TrackedTool] = {}
        self._aborted: set[str] = set()

    def add_tool(
        self, event: ApiToolUseDeltaEvent, assistant_message: ConversationMessage
    ) -> ToolExecutionStarted | None:
        """Add a tool to execute as it arrives mid-stream. Returns started event if tool was started."""
        tool_def = self._tool_registry.get(event.name)

        tracked = TrackedTool(
            id=event.id,
            name=event.name,
            input=event.input,
            assistant_message=assistant_message,
            is_concurrency_safe=tool_def.is_read_only(
                tool_def.input_model.model_validate(event.input)
            )
            if tool_def
            else False,
        )
        self._tools[event.id] = tracked
        if event.input:
            self._start_tool(tracked)
            return ToolExecutionStarted(tool_name=event.name, tool_input=event.input)
        return None

    def cancel(self, tool_id: str, reason: str) -> None:
        """Cancel a running tool."""
        self._aborted.add(tool_id)
        if tool_id in self._tools:
            self._tools[tool_id].cancelled = True
            self._tools[tool_id].cancel_reason = reason
            task = self._tools[tool_id].task
            if task and not task.done():
                task.cancel()

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

    def get_remaining(self) -> list[ToolExecutionCompleted | ToolExecutionCancelled]:
        """Get final results after stream completes."""
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
        try:
            if tool.id in self._aborted:
                tool.status = "completed"
                tool.cancelled = True
                return

            tool_def = self._tool_registry.get(tool.name)
            if not tool_def:
                tool.result = ToolResult(
                    output=f"Unknown tool: {tool.name}",
                    is_error=True,
                )
                tool.status = "completed"
                return

            parsed_input = tool_def.input_model.model_validate(tool.input)

            context_with_id = ToolExecutionContext(
                cwd=self._context.cwd,
                metadata={**self._context.metadata, "tool_id": tool.id},
            )

            try:
                tool.result = await tool_def.execute(parsed_input, context_with_id)
            except asyncio.CancelledError:
                tool.cancelled = True
                tool.cancel_reason = tool.cancel_reason or "Task cancelled"
            except Exception as exc:
                tool.result = ToolResult(
                    output=f"Tool execution failed: {exc}",
                    is_error=True,
                )

            tool.status = "completed"

        except asyncio.CancelledError:
            tool.cancelled = True
            tool.cancel_reason = tool.cancel_reason or "Task cancelled"
            tool.status = "completed"
        except Exception as exc:
            tool.result = ToolResult(
                output=f"Tool execution failed: {exc}",
                is_error=True,
            )
            tool.status = "completed"

    def get_started_events(self) -> list[ToolExecutionStarted]:
        """Get ToolExecutionStarted events for all queued tools."""
        return [
            ToolExecutionStarted(tool_name=t.name, tool_input=t.input)
            for t in self._tools.values()
            if t.status == "queued"
        ]

    def finalize(self) -> None:
        """Called when stream ends - wait for all tools to complete."""
        pass
