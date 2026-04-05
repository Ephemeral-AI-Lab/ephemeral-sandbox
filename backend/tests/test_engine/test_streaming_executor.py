"""Tests for StreamingToolExecutor - mid-stream tool detection and abort support."""

from __future__ import annotations

import asyncio
import json

import pytest
from pydantic import BaseModel, Field

from engine.messages import ConversationMessage, TextBlock
from engine.stream_events import (
    ToolExecutionCancelled,
    ToolExecutionCompleted,
    ToolExecutionProgress,
)
from engine.streaming_executor import StreamingToolExecutor, TrackedTool
from models.types import ApiToolUseDeltaEvent
from tools.base import BaseTool, BaseToolkit, ToolExecutionContext, ToolRegistry, ToolResult


# ---------------------------------------------------------------------------
# Fixtures: fake tools for testing
# ---------------------------------------------------------------------------


class SlowInput(BaseModel):
    message: str = Field(description="Message to process slowly")


class SlowTool(BaseTool):
    """A tool that takes a long time to execute."""

    name = "slow"
    description = "Takes a long time."
    input_model = SlowInput

    async def execute(self, arguments: SlowInput, context: ToolExecutionContext) -> ToolResult:
        await asyncio.sleep(10)  # Simulates long-running operation
        return ToolResult(output=f"processed: {arguments.message}")


class FastInput(BaseModel):
    value: int = Field(description="A number")


class FastTool(BaseTool):
    """A tool that executes quickly."""

    name = "fast"
    description = "Executes quickly."
    input_model = FastInput

    async def execute(self, arguments: FastInput, context: ToolExecutionContext) -> ToolResult:
        return ToolResult(output=json.dumps({"doubled": arguments.value * 2}))


class ProgressInput(BaseModel):
    lines: int = Field(description="Number of progress lines to emit")


class ProgressTool(BaseTool):
    """A tool that streams progress updates."""

    name = "progress"
    description = "Emits progress lines."
    input_model = ProgressInput

    async def execute(self, arguments: ProgressInput, context: ToolExecutionContext) -> ToolResult:
        results = []
        for i in range(arguments.lines):
            await asyncio.sleep(0.01)
            results.append(f"line {i}")
        return ToolResult(output=json.dumps({"lines": results}))


def _make_toolkit(*tools: BaseTool) -> BaseToolkit:
    return BaseToolkit(name="test_toolkit", description="Test", tools=list(tools))


def _make_registry(*tools: BaseTool) -> ToolRegistry:
    registry = ToolRegistry()
    registry.register_toolkit(_make_toolkit(*tools))
    return registry


def _make_context() -> ToolExecutionContext:
    return ToolExecutionContext(cwd="/tmp", metadata={})


def _make_assistant_msg() -> ConversationMessage:
    return ConversationMessage(role="assistant", content=[])


# ---------------------------------------------------------------------------
# Tests: TrackedTool dataclass
# ---------------------------------------------------------------------------


def test_tracked_tool_defaults():
    """TrackedTool has correct default values."""
    tracked = TrackedTool(
        id="tool_01",
        name="test",
        input={},
        assistant_message=_make_assistant_msg(),
    )
    assert tracked.status == "queued"
    assert tracked.is_concurrency_safe is True
    assert tracked.task is None
    assert tracked.progress_lines == []
    assert tracked.result is None
    assert tracked.cancelled is False
    assert tracked.cancel_reason == ""


# ---------------------------------------------------------------------------
# Tests: StreamingToolExecutor.add_tool
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_add_tool_starts_execution():
    """Adding a tool starts execution immediately."""
    registry = _make_registry(FastTool())
    context = _make_context()
    executor = StreamingToolExecutor(registry, context)

    event = ApiToolUseDeltaEvent(
        id="tool_01",
        name="fast",
        input={"value": 21},
    )

    executor.add_tool(event, _make_assistant_msg())

    # Tool should be executing
    assert "tool_01" in executor._tools
    assert executor._tools["tool_01"].status == "executing"
    assert executor._tools["tool_01"].name == "fast"


@pytest.mark.asyncio
async def test_add_tool_unknown_tool():
    """Adding a tool with unknown name marks it as completed with error."""
    registry = _make_registry()
    context = _make_context()
    executor = StreamingToolExecutor(registry, context)

    event = ApiToolUseDeltaEvent(
        id="tool_01",
        name="nonexistent",
        input={"any": "value"},
    )

    executor.add_tool(event, _make_assistant_msg())

    # Wait for execution to complete - unknown tools fail immediately
    await asyncio.sleep(0.1)
    tracked = executor._tools["tool_01"]
    assert tracked.status == "completed"


# ---------------------------------------------------------------------------
# Tests: StreamingToolExecutor.cancel
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_cancel_stops_running_tool():
    """Cancelling a tool stops its execution."""
    registry = _make_registry(SlowTool())
    context = _make_context()
    executor = StreamingToolExecutor(registry, context)

    event = ApiToolUseDeltaEvent(
        id="tool_01",
        name="slow",
        input={"message": "test"},
    )

    executor.add_tool(event, _make_assistant_msg())

    # Cancel immediately
    await asyncio.sleep(0.01)  # Let execution start
    executor.cancel("tool_01", "Too slow")

    # Wait for cancellation to propagate
    await asyncio.sleep(0.1)

    tracked = executor._tools["tool_01"]
    assert tracked.cancelled is True
    assert tracked.cancel_reason == "Too slow"
    assert tracked.status == "completed"


@pytest.mark.asyncio
async def test_cancel_unknown_tool_is_noop():
    """Cancelling unknown tool does nothing."""
    registry = _make_registry()
    context = _make_context()
    executor = StreamingToolExecutor(registry, context)

    # Should not raise
    executor.cancel("nonexistent", "reason")

    assert "nonexistent" not in executor._tools


# ---------------------------------------------------------------------------
# Tests: StreamingToolExecutor.get_remaining
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_get_remaining_returns_completed_tools():
    """get_remaining returns results of completed tools."""
    registry = _make_registry(FastTool())
    context = _make_context()
    executor = StreamingToolExecutor(registry, context)

    event = ApiToolUseDeltaEvent(
        id="tool_01",
        name="fast",
        input={"value": 10},
    )

    executor.add_tool(event, _make_assistant_msg())

    # Wait for completion
    await asyncio.sleep(0.1)

    results = executor.get_remaining()

    assert len(results) == 1
    assert isinstance(results[0], ToolExecutionCompleted)
    assert results[0].tool_name == "fast"


@pytest.mark.asyncio
async def test_get_remaining_returns_cancelled_tools():
    """get_remaining returns cancelled status for aborted tools."""
    registry = _make_registry(SlowTool())
    context = _make_context()
    executor = StreamingToolExecutor(registry, context)

    event = ApiToolUseDeltaEvent(
        id="tool_01",
        name="slow",
        input={"message": "test"},
    )

    executor.add_tool(event, _make_assistant_msg())
    await asyncio.sleep(0.01)
    executor.cancel("tool_01", "Aborted by LLM")

    # Wait for cancellation
    await asyncio.sleep(0.1)

    results = executor.get_remaining()

    assert len(results) == 1
    assert isinstance(results[0], ToolExecutionCancelled)
    assert results[0].tool_id == "tool_01"
    assert results[0].reason == "Aborted by LLM"


# ---------------------------------------------------------------------------
# Tests: Mid-stream tool detection
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_executor_tracks_multiple_tools():
    """Executor can track multiple tools simultaneously."""
    registry = _make_registry(FastTool(), FastTool())
    context = _make_context()
    executor = StreamingToolExecutor(registry, context)

    event1 = ApiToolUseDeltaEvent(id="tool_01", name="fast", input={"value": 1})
    event2 = ApiToolUseDeltaEvent(id="tool_02", name="fast", input={"value": 2})

    executor.add_tool(event1, _make_assistant_msg())
    executor.add_tool(event2, _make_assistant_msg())

    assert len(executor._tools) == 2
    assert "tool_01" in executor._tools
    assert "tool_02" in executor._tools


@pytest.mark.asyncio
async def test_multiple_tools_run_concurrently():
    """Multiple tools run concurrently when added in sequence."""
    registry = _make_registry(FastTool())
    context = _make_context()
    executor = StreamingToolExecutor(registry, context)

    # Add 3 fast tools
    for i in range(3):
        event = ApiToolUseDeltaEvent(
            id=f"tool_{i:02d}",
            name="fast",
            input={"value": i},
        )
        executor.add_tool(event, _make_assistant_msg())

    # All should be tracked
    assert len(executor._tools) == 3
    assert all(t.name == "fast" for t in executor._tools.values())

    # All should complete eventually
    await asyncio.sleep(0.1)
    results = executor.get_remaining()
    assert len(results) == 3


# ---------------------------------------------------------------------------
# Tests: Progress streaming
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_get_progress_returns_empty_initially():
    """get_progress returns empty list before any tools complete."""
    registry = _make_registry(FastTool())
    context = _make_context()
    executor = StreamingToolExecutor(registry, context)

    event = ApiToolUseDeltaEvent(
        id="tool_01",
        name="fast",
        input={"value": 10},
    )
    executor.add_tool(event, _make_assistant_msg())

    progress = executor.get_progress()
    assert progress == []
