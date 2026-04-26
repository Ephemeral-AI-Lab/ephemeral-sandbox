"""Tests for direct tool execution helpers."""

from __future__ import annotations

from pathlib import Path

import pytest
from pydantic import BaseModel, RootModel

from engine.core.query import QueryContext
from engine.core.streaming_executor import StreamingToolExecutor
from message.stream_events import StreamEvent, ToolExecutionStarted
from providers.types import ApiToolUseDeltaEvent, SupportsStreamingMessages
from tools.core.base import (
    BaseTool,
    ToolExecutionContextService,
    ToolRegistry,
    ToolResult,
)
from tools.core.tool_execution import execute_tool_call_streaming, execute_tool_once

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

    async def execute(self, arguments: _Args, context: ToolExecutionContextService) -> ToolResult:
        del context
        self.seen.append(arguments.value)
        return ToolResult(output=arguments.value)


class _FailingTool(BaseTool):
    name = "failing_tool"
    description = "Returns a failed tool result."
    input_model = _Args
    output_model = _Out

    async def execute(self, arguments: _Args, context: ToolExecutionContextService) -> ToolResult:
        del arguments, context
        return ToolResult(output="tool failed", is_error=True, metadata={"status": "failed"})


class _TerminalEchoTool(_EchoTool):
    name = "terminal_echo"
    is_terminal_tool = True


class _TerminalFailingTool(_FailingTool):
    name = "terminal_failing"
    is_terminal_tool = True


class _FakeClient(SupportsStreamingMessages):
    async def stream_message(self, request):  # pragma: no cover - not used
        if False:
            yield None


async def _capture_emit(events: list[StreamEvent], event: StreamEvent) -> None:
    events.append(event)


def _context() -> ToolExecutionContextService:
    return ToolExecutionContextService(cwd=Path("/tmp"))


async def test_tool_execution_context_service_unfolds_metadata_fields() -> None:
    svc = object()
    context = ToolExecutionContextService(
        cwd="/tmp",
        services={"ci_service": svc, "agent_name": "worker", "custom": "value"},
    )

    assert context.cwd == Path("/tmp")
    assert context.ci_service is svc
    assert context.agent_name == "worker"
    assert context.get("custom") == "value"

    context.sandbox_id = "sandbox-1"
    context["task_id"] = "task-1"

    assert context.sandbox_id == "sandbox-1"
    assert context["task_id"] == "task-1"
    assert isinstance(context, ToolExecutionContextService)


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


async def test_execute_tool_once_emits_started_and_executes_tool() -> None:
    tool = _EchoTool()
    events: list[StreamEvent] = []

    result = await execute_tool_once(
        tool,
        {"value": "hello"},
        _context(),
        emit=lambda event: _capture_emit(events, event),
    )

    assert result.is_error is False
    assert result.output == "hello"
    assert tool.seen == ["hello"]
    assert [type(event) for event in events] == [ToolExecutionStarted]
    assert isinstance(events[0], ToolExecutionStarted)
    assert events[0].tool_input == {"value": "hello"}


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


async def test_execute_tool_once_stamps_does_terminate_on_terminal_success() -> None:
    tool = _TerminalEchoTool()
    result = await execute_tool_once(
        tool,
        {"value": "hi"},
        _context(),
        emit=lambda event: _capture_emit([], event),
    )
    assert result.is_error is False
    assert result.does_terminate is True


async def test_execute_tool_once_skips_does_terminate_on_terminal_error() -> None:
    tool = _TerminalFailingTool()
    result = await execute_tool_once(
        tool,
        {"value": "hi"},
        _context(),
        emit=lambda event: _capture_emit([], event),
    )
    assert result.is_error is True
    assert result.does_terminate is False


async def test_execute_tool_once_skips_does_terminate_for_non_terminal_tool() -> None:
    tool = _EchoTool()
    result = await execute_tool_once(
        tool,
        {"value": "hi"},
        _context(),
        emit=lambda event: _capture_emit([], event),
    )
    assert result.is_error is False
    assert result.does_terminate is False


async def test_execute_tool_call_streaming_propagates_does_terminate_to_block() -> None:
    tool = _TerminalEchoTool()
    context = _query_context(tool)
    result = await execute_tool_call_streaming(
        context,
        "terminal_echo",
        "toolu_term",
        {"value": "bye"},
        emit=lambda event: _capture_emit([], event),
    )
    assert result.is_error is False
    assert result.does_terminate is True


async def test_streaming_executor_propagates_terminal_completion_marker() -> None:
    registry = ToolRegistry()
    registry.register(_TerminalEchoTool())
    executor = StreamingToolExecutor(
        registry,
        ToolExecutionContextService(cwd=Path("/tmp")),
    )

    executor.add_tool(
        ApiToolUseDeltaEvent(
            id="toolu_streamed_terminal",
            name="terminal_echo",
            input={"value": "done"},
        )
    )

    results = await executor.get_remaining()

    assert len(results) == 1
    assert results[0].does_terminate is True
