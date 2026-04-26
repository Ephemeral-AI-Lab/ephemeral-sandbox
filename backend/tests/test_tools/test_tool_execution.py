"""Tests for direct tool execution helpers."""

from __future__ import annotations

import asyncio
import json
from pathlib import Path

import pytest
from pydantic import BaseModel, RootModel

from engine.core.query import QueryContext, run_query
from engine.core.streaming_executor import StreamingToolExecutor
from engine.runtime.background_dispatch import launch_background_tool
from engine.runtime.background_tasks import BackgroundTaskManager
from message.messages import ConversationMessage, ToolUseBlock
from message.stream_events import StreamEvent, SystemNotification, ToolExecutionStarted
from providers.types import (
    ApiMessageCompleteEvent,
    ApiToolUseDeltaEvent,
    SupportsStreamingMessages,
    UsageSnapshot,
)
from tools.core.base import (
    BaseTool,
    ToolExecutionContextService,
    ToolRegistry,
    ToolResult,
)
from tools.core.decorator import tool
from tools.core.hooks import HookResult
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


class _SequentialPreHook:
    target_tool = "echo_tool"

    def __init__(self, label: str, order: list[str]) -> None:
        self.label = label
        self.order = order

    async def run(
        self,
        tool_input: _Args,
        context: ToolExecutionContextService,
    ) -> HookResult[_Args]:
        del context
        self.order.append(self.label)
        return HookResult.pass_(_Args(value=f"{tool_input.value}-{self.label}"))


class _FailPreHook:
    target_tool = "echo_tool"

    async def run(
        self,
        tool_input: _Args,
        context: ToolExecutionContextService,
    ) -> HookResult[_Args]:
        del tool_input, context
        return HookResult.fail("prehook denied for test")


class _InvalidPreHook:
    target_tool = "echo_tool"

    async def run(
        self,
        tool_input: _Args,
        context: ToolExecutionContextService,
    ) -> HookResult[_Args]:
        del tool_input, context
        return HookResult.pass_({"wrong": "shape"})  # type: ignore[arg-type]


class _ExceptionPreHook:
    target_tool = "echo_tool"

    async def run(
        self,
        tool_input: _Args,
        context: ToolExecutionContextService,
    ) -> HookResult[_Args]:
        del tool_input, context
        raise RuntimeError("boom")


class _AppendPostHook:
    target_tool = "echo_tool"

    async def run(
        self,
        tool_input: _Args,
        result: ToolResult,
        context: ToolExecutionContextService,
    ) -> HookResult[ToolResult]:
        del tool_input, context
        return HookResult.pass_(ToolResult(output=f"{result.output}!"))


class _FailPostHook:
    target_tool = "echo_tool"

    async def run(
        self,
        tool_input: _Args,
        result: ToolResult,
        context: ToolExecutionContextService,
    ) -> HookResult[ToolResult]:
        del tool_input, result, context
        return HookResult.fail("posthook rejected result")


class _InvalidPostHook:
    target_tool = "echo_tool"

    async def run(
        self,
        tool_input: _Args,
        result: ToolResult,
        context: ToolExecutionContextService,
    ) -> HookResult[ToolResult]:
        del tool_input, result, context
        return HookResult.pass_(ToolResult(output={"not": "a string"}))  # type: ignore[arg-type]


class _NotifyPreHook:
    target_tool = "echo_tool"

    async def run(
        self,
        tool_input: _Args,
        context: ToolExecutionContextService,
    ) -> HookResult[_Args]:
        await context.notify_system("hook note", category="hook_test")
        return HookResult.pass_(tool_input)


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


async def test_prehooks_run_sequentially_and_mutate_input() -> None:
    order: list[str] = []
    tool = _EchoTool()
    tool.pre_hooks = (
        _SequentialPreHook("a", order),
        _SequentialPreHook("b", order),
    )
    events: list[StreamEvent] = []

    result = await execute_tool_once(
        tool,
        {"value": "start"},
        _context(),
        emit=lambda event: _capture_emit(events, event),
    )

    assert result.is_error is False
    assert result.output == "start-a-b"
    assert tool.seen == ["start-a-b"]
    assert order == ["a", "b"]
    assert isinstance(events[0], ToolExecutionStarted)
    assert events[0].tool_input == {"value": "start-a-b"}
    assert result.metadata["effective_tool_input"] == {"value": "start-a-b"}


async def test_prehook_fail_blocks_tool_and_returns_reason() -> None:
    tool = _EchoTool()
    tool.pre_hooks = (_FailPreHook(),)

    result = await execute_tool_once(
        tool,
        {"value": "hello"},
        _context(),
        emit=lambda event: _capture_emit([], event),
    )

    payload = json.loads(result.output)
    assert result.is_error is True
    assert tool.seen == []
    assert payload["hookSpecificOutput"]["hookEventName"] == "PreToolUse"
    assert payload["hookSpecificOutput"]["permissionDecision"] == "deny"
    assert payload["hookSpecificOutput"]["permissionDecisionReason"] == (
        "prehook denied for test"
    )
    assert result.metadata["hook_failure"]["reason"] == "prehook denied for test"


async def test_invalid_prehook_mutation_returns_hook_failure() -> None:
    tool = _EchoTool()
    tool.pre_hooks = (_InvalidPreHook(),)

    result = await execute_tool_once(
        tool,
        {"value": "hello"},
        _context(),
        emit=lambda event: _capture_emit([], event),
    )

    assert result.is_error is True
    assert "inconsistent with _Args" in result.metadata["hook_failure"]["reason"]
    assert "value: Field required" in result.metadata["hook_failure"]["reason"]
    assert tool.seen == []


async def test_posthook_mutates_result_and_schema_validation_passes() -> None:
    tool = _EchoTool()
    tool.post_hooks = (_AppendPostHook(),)

    result = await execute_tool_once(
        tool,
        {"value": "hello"},
        _context(),
        emit=lambda event: _capture_emit([], event),
    )

    assert result.is_error is False
    assert result.output == "hello!"
    assert tool.seen == ["hello"]


async def test_invalid_posthook_mutation_returns_hook_failure() -> None:
    tool = _EchoTool()
    tool.post_hooks = (_InvalidPostHook(),)

    result = await execute_tool_once(
        tool,
        {"value": "hello"},
        _context(),
        emit=lambda event: _capture_emit([], event),
    )

    assert result.is_error is True
    assert "inconsistent with _Out" in result.metadata["hook_failure"]["reason"]
    assert tool.seen == ["hello"]


async def test_posthook_fail_replaces_result_with_hook_failure() -> None:
    tool = _EchoTool()
    tool.post_hooks = (_FailPostHook(),)

    result = await execute_tool_once(
        tool,
        {"value": "hello"},
        _context(),
        emit=lambda event: _capture_emit([], event),
    )

    payload = json.loads(result.output)
    assert result.is_error is True
    assert tool.seen == ["hello"]
    assert payload["hookSpecificOutput"]["hookEventName"] == "PostToolUse"
    assert payload["hookSpecificOutput"]["permissionDecisionReason"] == (
        "posthook rejected result"
    )


async def test_hook_exception_becomes_hook_failure() -> None:
    tool = _EchoTool()
    tool.pre_hooks = (_ExceptionPreHook(),)

    result = await execute_tool_once(
        tool,
        {"value": "hello"},
        _context(),
        emit=lambda event: _capture_emit([], event),
    )

    assert result.is_error is True
    assert "RuntimeError: boom" in result.metadata["hook_failure"]["reason"]
    assert tool.seen == []


async def test_hook_notification_uses_fallback_service_and_records_reminder() -> None:
    tool = _EchoTool()
    tool.pre_hooks = (_NotifyPreHook(),)
    events: list[StreamEvent] = []

    result = await execute_tool_once(
        tool,
        {"value": "hello"},
        _context(),
        emit=lambda event: _capture_emit(events, event),
    )

    notifications = [event for event in events if isinstance(event, SystemNotification)]
    assert result.is_error is False
    assert len(notifications) == 1
    assert notifications[0].text == "hook note"
    assert notifications[0].category == "hook_test"
    assert result.metadata["system_reminders"][0]["text"] == "hook note"


async def test_decorator_attaches_tool_hooks() -> None:
    hook = _NotifyPreHook()

    @tool(
        name="echo_tool",
        description="decorated",
        input_model=_Args,
        output_model=_Out,
        pre_hooks=[hook],
    )
    async def decorated_echo(value: str, *, context: ToolExecutionContextService) -> ToolResult:
        del value, context
        return ToolResult(output="ok")

    assert decorated_echo.pre_hooks == (hook,)


async def test_decorator_rejects_mismatched_hook_target() -> None:
    class _WrongHook(_NotifyPreHook):
        target_tool = "other_tool"

    with pytest.raises(ValueError, match="expected target_tool='echo_tool'"):

        @tool(
            name="echo_tool",
            description="decorated",
            input_model=_Args,
            output_model=_Out,
            pre_hooks=[_WrongHook()],
        )
        async def decorated_echo(
            value: str,
            *,
            context: ToolExecutionContextService,
        ) -> ToolResult:
            del value, context
            return ToolResult(output="ok")


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


async def test_query_loop_appends_hook_notification_as_system_reminder() -> None:
    class _HookNotificationClient(SupportsStreamingMessages):
        def __init__(self) -> None:
            self.requests = []

        async def stream_message(self, request):
            self.requests.append(request)
            if len(self.requests) == 1:
                yield ApiMessageCompleteEvent(
                    message=ConversationMessage(
                        role="assistant",
                        content=[
                            ToolUseBlock(
                                id="toolu_notify",
                                name="echo_tool",
                                input={"value": "hi"},
                            )
                        ],
                    ),
                    usage=UsageSnapshot(),
                )
            else:
                yield ApiMessageCompleteEvent(
                    message=ConversationMessage(role="assistant", content=[]),
                    usage=UsageSnapshot(),
                )

    tool = _EchoTool()
    tool.pre_hooks = (_NotifyPreHook(),)
    registry = ToolRegistry()
    registry.register(tool)
    client = _HookNotificationClient()
    context = QueryContext(
        api_client=client,
        tool_registry=registry,
        cwd=Path("/tmp"),
        model="test",
        system_prompt="",
        max_tokens=100,
    )

    display_messages, stream = await run_query(context, [])
    events = []
    async for event, _usage in stream:
        events.append(event)

    assert any(
        isinstance(event, SystemNotification) and event.text == "hook note"
        for event in events
    )
    assert any(
        message.system_reminder_text == "hook note"
        for message in display_messages
    )
    assert len(client.requests) == 2
    assert any(
        message.system_reminder_text == "hook note"
        for message in client.requests[1].messages
    )


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


async def test_background_tool_runs_hooks_and_reports_failure() -> None:
    class _BackgroundEchoTool(_EchoTool):
        name = "background_echo"
        background = "optional"

    class _BackgroundFailPreHook(_FailPreHook):
        target_tool = "background_echo"

    tool = _BackgroundEchoTool()
    tool.pre_hooks = (_BackgroundFailPreHook(),)
    registry = ToolRegistry()
    registry.register(tool)
    context = _query_context(tool)
    manager = BackgroundTaskManager()

    async def _execute_tool_call(
        tool_name: str,
        tool_use_id: str,
        tool_input: dict[str, object],
        extra_metadata=None,
    ):
        return await execute_tool_call_streaming(
            context,
            tool_name,
            tool_use_id,
            tool_input,
            emit=lambda event: _capture_emit([], event),
            extra_metadata=extra_metadata,
        )

    tool_result, bg_event, reject_event = launch_background_tool(
        tool_registry=registry,
        tool_metadata=context.tool_metadata,
        cwd=Path("/tmp"),
        background_manager=manager,
        tool_use=ToolUseBlock(
            id="toolu_bg",
            name="background_echo",
            input={"value": "hi", "background": True},
        ),
        execute_tool_call=_execute_tool_call,
    )

    assert tool_result.is_error is False
    assert bg_event is not None
    assert reject_event is None

    completed = []
    for _ in range(20):
        await asyncio.sleep(0.01)
        completed = manager.collect_completed()
        if completed:
            break

    assert completed
    assert completed[0].result is not None
    assert completed[0].result.is_error is True
    payload = json.loads(completed[0].result.output)
    assert payload["hookSpecificOutput"]["permissionDecisionReason"] == (
        "prehook denied for test"
    )
    assert tool.seen == []
