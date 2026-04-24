"""Tests for StreamingToolExecutor - mid-stream tool detection and abort support."""

from __future__ import annotations

import asyncio
import json

import pytest
from pydantic import BaseModel, Field

from message.stream_events import (
    ToolExecutionCancelled,
    ToolExecutionCompleted,
    ToolExecutionStarted,
)
from engine.core.streaming_executor import (
    StreamingToolExecutor,
    TrackedTool,
    defer_background_dispatch,
)
from providers.types import ApiToolUseDeltaEvent
from team.core.models import Plan
from tools.core.base import BaseTool, ToolExecutionContext, ToolRegistry, ToolResult


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


class AtlasInput(BaseModel):
    subsystem: str = Field(description="Subsystem to inspect")


class AtlasTool(BaseTool):
    """A fake atlas tool that returns lookup metadata."""

    name = "atlas_lookup"
    description = "Returns atlas lookup metadata."
    input_model = AtlasInput

    async def execute(self, arguments: AtlasInput, context: ToolExecutionContext) -> ToolResult:
        return ToolResult(
            output="atlas_lookup: use=1 refresh=0 scout=0",
            metadata={
                "lookups": [
                    {
                        "subsystem": arguments.subsystem,
                        "action": "use",
                        "staged_artifact_ref": "atlas:pydantic/networks.py:deadbeef",
                        "staleness_reason": None,
                    }
                ]
            },
        )


def _make_registry(*tools: BaseTool) -> ToolRegistry:
    registry = ToolRegistry()
    registry.register_many(tools)
    return registry


def _make_context() -> ToolExecutionContext:
    return ToolExecutionContext(cwd="/tmp", metadata={})


# ---------------------------------------------------------------------------
# Tests: TrackedTool dataclass
# ---------------------------------------------------------------------------


def test_tracked_tool_defaults():
    """TrackedTool has correct default values."""
    tracked = TrackedTool(
        id="tool_01",
        name="test",
        input={},
    )
    assert tracked.status == "queued"
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

    executor.add_tool(event)

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

    executor.add_tool(event)

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

    executor.add_tool(event)

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

    executor.add_tool(event)

    # Wait for completion
    await asyncio.sleep(0.1)

    results = await executor.get_remaining()

    assert len(results) == 1
    assert isinstance(results[0], ToolExecutionCompleted)
    assert results[0].tool_name == "fast"


@pytest.mark.asyncio
async def test_get_remaining_preserves_tool_metadata() -> None:
    registry = _make_registry(AtlasTool())
    context = _make_context()
    executor = StreamingToolExecutor(registry, context)

    event = ApiToolUseDeltaEvent(
        id="tool_01",
        name="atlas_lookup",
        input={"subsystem": "pydantic/networks.py"},
    )

    executor.add_tool(event)
    await asyncio.sleep(0.1)

    results = await executor.get_remaining()

    assert len(results) == 1
    assert isinstance(results[0], ToolExecutionCompleted)
    assert results[0].metadata["lookups"][0]["subsystem"] == "pydantic/networks.py"


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

    executor.add_tool(event)
    await asyncio.sleep(0.01)
    executor.cancel("tool_01", "Aborted by LLM")

    # Wait for cancellation
    await asyncio.sleep(0.1)

    results = await executor.get_remaining()

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

    executor.add_tool(event1)
    executor.add_tool(event2)

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
        executor.add_tool(event)

    # All should be tracked
    assert len(executor._tools) == 3
    assert all(t.name == "fast" for t in executor._tools.values())

    # All should complete eventually
    await asyncio.sleep(0.1)
    results = await executor.get_remaining()
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
    executor.add_tool(event)

    progress = executor.get_progress()
    assert progress == []


# ---------------------------------------------------------------------------
# Tests: Background tool skipping
# ---------------------------------------------------------------------------


class BgBashInput(BaseModel):
    command: str = Field(description="Command to run")


class BgBashTool(BaseTool):
    """A tool that supports background execution."""

    name = "daytona_shell"
    description = "Run a command in the sandbox."
    input_model = BgBashInput
    background = "optional"  # LLM may opt in via input.background=true

    async def execute(self, arguments: BgBashInput, context: ToolExecutionContext) -> ToolResult:
        return ToolResult(output=f"ran: {arguments.command}")


class SubmitPlanInput(BaseModel):
    items: list[dict[str, object]] = Field(description="Plan items to submit")


class SubmitPlanTool(BaseTool):
    """A tool that simulates a submission tool writing accepted metadata."""

    name = "submit_plan"
    description = "Submit a plan."
    input_model = SubmitPlanInput

    async def execute(
        self, arguments: SubmitPlanInput, context: ToolExecutionContext
    ) -> ToolResult:
        key = context.metadata.get("submission_metadata_key", "submitted_plan")
        context.metadata[key] = {"items": arguments.items}
        return ToolResult(output="Plan accepted")


class SubmitTaskSummaryInput(BaseModel):
    type: str = Field(description="Summary type")
    content: str = Field(description="Summary content")


class SubmitTaskSummaryTool(BaseTool):
    """A tool that simulates terminal summary submission metadata."""

    name = "submit_task_success"
    description = "Submit a task summary."
    input_model = SubmitTaskSummaryInput

    async def execute(
        self, arguments: SubmitTaskSummaryInput, context: ToolExecutionContext
    ) -> ToolResult:
        context.metadata["task_summary_type"] = arguments.type
        context.metadata["task_summary"] = arguments.content
        return ToolResult(output="Summary accepted")


class SubmitResolvedPlanInput(BaseModel):
    spec: dict[str, str] = Field(description="Spec for a single planned task")


class SubmitResolvedPlanTool(BaseTool):
    """A tool that simulates planner submission metadata."""

    name = "submit_resolved_plan"
    description = "Submit resolved plan metadata."
    input_model = SubmitResolvedPlanInput

    async def execute(
        self, arguments: SubmitResolvedPlanInput, context: ToolExecutionContext
    ) -> ToolResult:
        context.metadata["resolved_plan"] = Plan.from_dict(
            {"tasks": [{"id": "dev-1", "spec": arguments.spec, "agent": "developer"}]}
        )
        context.metadata["plan_is_replan"] = False
        return ToolResult(output="Plan accepted")


@pytest.mark.asyncio
async def test_add_tool_skips_background_tool():
    """add_tool skips tools with background=True when tool supports background."""
    registry = _make_registry(BgBashTool())
    context = _make_context()
    executor = StreamingToolExecutor(
        registry, context, should_defer=defer_background_dispatch
    )

    event = ApiToolUseDeltaEvent(
        id="tool_bg",
        name="daytona_shell",
        input={"command": "sleep 10", "background": True},
    )

    started = executor.add_tool(event)

    assert started is None, "Background tool should not produce a started event"
    assert "tool_bg" not in executor._tools, "Background tool should not be tracked"
    assert "tool_bg" in executor.deferred_dispatch_ids


@pytest.mark.asyncio
async def test_add_tool_runs_foreground_when_background_false():
    """add_tool starts tools normally when background is False or absent."""
    registry = _make_registry(BgBashTool())
    context = _make_context()
    executor = StreamingToolExecutor(registry, context)

    event = ApiToolUseDeltaEvent(
        id="tool_fg",
        name="daytona_shell",
        input={"command": "echo hello", "background": False},
    )

    started = executor.add_tool(event)

    assert started is None
    assert "tool_fg" in executor._tools
    assert len(executor.deferred_dispatch_ids) == 0
    await asyncio.sleep(0)
    events = executor.get_events()
    assert any(isinstance(ev, ToolExecutionStarted) for ev in events)


@pytest.mark.asyncio
async def test_add_tool_runs_non_bg_tool_with_background_flag():
    """add_tool runs tools that don't support background even if background=True is sent."""
    registry = _make_registry(FastTool())
    context = _make_context()
    executor = StreamingToolExecutor(registry, context)

    event = ApiToolUseDeltaEvent(
        id="tool_01",
        name="fast",
        input={"value": 5, "background": True},
    )

    started = executor.add_tool(event)

    assert started is None
    assert "tool_01" in executor._tools
    assert len(executor.deferred_dispatch_ids) == 0
    await asyncio.sleep(0)
    events = executor.get_events()
    assert any(isinstance(ev, ToolExecutionStarted) for ev in events)


@pytest.mark.asyncio
async def test_mixed_foreground_and_background_tools():
    """Executor handles a mix of foreground and background tools in the same turn."""
    registry = _make_registry(BgBashTool(), FastTool())
    context = _make_context()
    executor = StreamingToolExecutor(
        registry, context, should_defer=defer_background_dispatch
    )

    bg_event = ApiToolUseDeltaEvent(
        id="tool_bg",
        name="daytona_shell",
        input={"command": "sleep 10", "background": True},
    )
    fg_event = ApiToolUseDeltaEvent(
        id="tool_fg",
        name="fast",
        input={"value": 42},
    )

    bg_started = executor.add_tool(bg_event)
    fg_started = executor.add_tool(fg_event)

    assert bg_started is None, "Background tool should be skipped"
    assert fg_started is None

    assert len(executor._tools) == 1
    assert "tool_fg" in executor._tools
    assert "tool_bg" in executor.deferred_dispatch_ids

    await asyncio.sleep(0.1)
    events = executor.get_events()
    assert any(isinstance(ev, ToolExecutionStarted) for ev in events)
    results = await executor.get_remaining()
    assert len(results) == 1
    assert results[0].tool_name == "fast"


@pytest.mark.asyncio
async def test_submit_tool_propagates_submission_metadata_to_live_context():
    """Streaming execution must preserve accepted submissions."""
    registry = _make_registry(SubmitPlanTool())
    context = ToolExecutionContext(
        cwd="/tmp",
        metadata={"submission_metadata_key": "submitted_plan"},
    )
    executor = StreamingToolExecutor(registry, context)

    event = ApiToolUseDeltaEvent(
        id="tool_submit",
        name="submit_plan",
        input={"items": [{"agent_name": "developer"}]},
    )

    executor.add_tool(event)

    await asyncio.sleep(0.1)
    await executor.get_remaining()

    assert context.metadata["submitted_plan"] == {
        "items": [{"agent_name": "developer"}]
    }


@pytest.mark.asyncio
async def test_submit_task_success_metadata_propagates_to_live_context():
    registry = _make_registry(SubmitTaskSummaryTool())
    context = _make_context()
    executor = StreamingToolExecutor(registry, context)

    event = ApiToolUseDeltaEvent(
        id="tool_summary",
        name="submit_task_success",
        input={"type": "success", "content": "Implemented the fix"},
    )

    executor.add_tool(event)

    await asyncio.sleep(0.1)
    await executor.get_remaining()

    assert context.metadata["task_summary_type"] == "success"
    assert context.metadata["task_summary"] == "Implemented the fix"


@pytest.mark.asyncio
async def test_resolved_plan_metadata_propagates_to_live_context():
    registry = _make_registry(SubmitResolvedPlanTool())
    context = _make_context()
    executor = StreamingToolExecutor(registry, context)

    event = ApiToolUseDeltaEvent(
        id="tool_plan",
        name="submit_resolved_plan",
        input={
            "spec": {
                "goal": "Fix the discriminator pipeline",
                "detail": "Repair the discriminator pipeline.",
                "acceptance_criteria": "Run the focused pipeline tests.",
            }
        },
    )

    executor.add_tool(event)

    await asyncio.sleep(0.1)
    await executor.get_remaining()

    resolved_plan = context.metadata["resolved_plan"]
    assert isinstance(resolved_plan, Plan)
    assert resolved_plan.tasks[0].spec.goal == "Fix the discriminator pipeline"
    assert context.metadata["plan_is_replan"] is False
