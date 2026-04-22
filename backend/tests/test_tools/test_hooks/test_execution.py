"""Integration tests for hook-aware tool execution helpers."""

from __future__ import annotations

from pathlib import Path

import pytest
from pydantic import BaseModel, RootModel

from engine.core.query import QueryContext
from message.stream_events import StreamEvent, SystemNotification, ToolExecutionStarted
from providers.types import SupportsStreamingMessages
from tools.core.base import BaseTool, ToolExecutionContext, ToolRegistry, ToolResult
from tools.core.hooks import PostHookOutcome, PreHookOutcome, default_registry
from tools.core.hooks.execution import execute_tool_with_hooks
from tools.core.tool_execution import execute_tool_call_streaming

pytestmark = pytest.mark.asyncio


class _Args(BaseModel):
    value: str


class _Out(RootModel[str]):
    pass


class _EchoTool(BaseTool):
    name = "echo_tool"
    description = "Echoes the value argument."
    input_model = _Args
    output_model = _Out

    def __init__(self) -> None:
        self.seen: list[str] = []

    async def execute(self, arguments: _Args, context: ToolExecutionContext) -> ToolResult:
        self.seen.append(arguments.value)
        return ToolResult(output=arguments.value)


class _FailingTool(BaseTool):
    name = "failing_tool"
    description = "Returns a failed tool result."
    input_model = _Args
    output_model = _Out

    async def execute(self, arguments: _Args, context: ToolExecutionContext) -> ToolResult:
        del arguments, context
        return ToolResult(output="tool failed", is_error=True, metadata={"status": "failed"})


class _FakeClient(SupportsStreamingMessages):
    async def stream_message(self, request):  # pragma: no cover - not used
        if False:
            yield None


@pytest.fixture(autouse=True)
def _clear_default_registry():
    registry = default_registry()
    existing_pre = list(registry._pre)
    existing_post = list(registry._post)
    registry.clear()
    yield
    registry.clear()
    registry._pre.extend(existing_pre)
    registry._post.extend(existing_post)


async def _capture_emit(events: list[StreamEvent], event: StreamEvent) -> None:
    events.append(event)


def _context() -> ToolExecutionContext:
    return ToolExecutionContext(cwd=Path("/tmp"))


def _query_context(tool: BaseTool) -> QueryContext:
    registry = ToolRegistry()
    registry.register(tool)
    return QueryContext(
        api_client=_FakeClient(),
        tool_registry=registry,
        cwd=Path("/tmp"),
        model="test",
        system_prompt="",
        max_tokens=100,
    )


async def test_execute_tool_with_hooks_mutates_and_emits_advisory() -> None:
    tool = _EchoTool()
    events: list[StreamEvent] = []

    async def mutate(tool_name, args, context):
        return PreHookOutcome(
            tool_input=_Args(value=args.value.upper()),
            advisories=("normalized",),
        )

    default_registry().register("echo_tool", "pre", 10, mutate, name="mutate")

    result = await execute_tool_with_hooks(
        tool,
        {"value": "hello"},
        _context(),
        emit=lambda event: _capture_emit(events, event),
    )

    assert result.is_error is False
    assert result.output == "HELLO"
    assert tool.seen == ["HELLO"]
    assert [type(event) for event in events] == [SystemNotification, ToolExecutionStarted]
    assert isinstance(events[0], SystemNotification)
    assert events[0].text == "[pre-hook tip] echo_tool: normalized"
    assert isinstance(events[1], ToolExecutionStarted)
    assert events[1].tool_input == {"value": "HELLO"}


async def test_execute_tool_with_hooks_pre_denial_skips_started_and_tool() -> None:
    tool = _EchoTool()
    events: list[StreamEvent] = []

    async def block(tool_name, args, context):
        return PreHookOutcome(has_error=True, error_message="no")

    default_registry().register("echo_tool", "pre", 10, block, name="block")

    result = await execute_tool_with_hooks(
        tool,
        {"value": "hello"},
        _context(),
        emit=lambda event: _capture_emit(events, event),
    )

    assert result.is_error is True
    assert result.output == "pre-hook blocked echo_tool: no"
    assert result.metadata == {"blocked_by": "pre_hook"}
    assert tool.seen == []
    assert events == []


async def test_execute_tool_with_hooks_post_denial_replaces_failed_tool_result() -> None:
    tool = _FailingTool()
    events: list[StreamEvent] = []

    async def block(tool_name, args, context, result):
        assert result.is_error is True
        return PostHookOutcome(has_error=True, error_message="post policy failed")

    default_registry().register("failing_tool", "post", 10, block, name="post-block")

    result = await execute_tool_with_hooks(
        tool,
        {"value": "hello"},
        _context(),
        emit=lambda event: _capture_emit(events, event),
    )

    assert result.is_error is True
    assert result.output == "post-hook failed failing_tool: post policy failed"
    assert result.metadata == {
        "status": "failed",
        "blocked_by": "post_hook",
        "original_tool_is_error": True,
    }
    assert [type(event) for event in events] == [ToolExecutionStarted]


async def test_execute_tool_call_streaming_returns_one_tool_result_block() -> None:
    tool = _EchoTool()
    events: list[StreamEvent] = []
    context = _query_context(tool)

    result = await execute_tool_call_streaming(
        context,
        "echo_tool",
        "toolu_1",
        {"value": "hi"},
        emit=lambda event: _capture_emit(events, event),
    )

    assert result.tool_use_id == "toolu_1"
    assert result.content == "hi"
    assert result.is_error is False
    assert [type(event) for event in events] == [ToolExecutionStarted]
