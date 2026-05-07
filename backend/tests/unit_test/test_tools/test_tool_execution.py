"""Tests for direct tool execution helpers."""

from __future__ import annotations

import asyncio
import json
from pathlib import Path

import pytest
from pydantic import BaseModel, RootModel

from engine.query.context import QueryContext, QueryExitReason
from engine.query.loop import run_query
from engine.tool_call.streaming import StreamingToolExecutor
from engine.background.dispatch import (
    launch_and_collect_bg_events,
    launch_background_tool,
)
from engine.background.manager import BackgroundTaskManager
from message.messages import (
    ConversationMessage,
    SystemNotificationBlock,
    ToolResultBlock,
    ToolUseBlock,
)
from message.stream_events import (
    StreamEvent,
    ToolExecutionCompleted,
    ToolExecutionStarted,
)
from notification._runtime import SystemNotification
from notification.library import make_budget_warning, make_opening_reminder
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
from tools.core.runtime import ExecutionMetadata
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


class _PreparedContextTool(_EchoTool):
    name = "prepared_context_tool"

    async def execute(self, arguments: _Args, context: ToolExecutionContextService) -> ToolResult:
        del arguments
        return ToolResult(output=str(context.get("prepared_by_test", "")))


class _ToolNotifyTool(_EchoTool):
    name = "tool_notify"

    async def execute(self, arguments: _Args, context: ToolExecutionContextService) -> ToolResult:
        await context.notify_system("tool note")
        return await super().execute(arguments, context)


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
        await context.notify_system("hook note")
        return HookResult.pass_(tool_input)


class _ConversationMessagesPreHook:
    target_tool = "echo_tool"

    async def run(
        self,
        tool_input: _Args,
        context: ToolExecutionContextService,
    ) -> HookResult[_Args]:
        messages = context.get("conversation_messages")
        assert isinstance(messages, list)
        return HookResult.pass_(_Args(value=f"{tool_input.value}:{len(messages)}"))


class _FakeClient(SupportsStreamingMessages):
    async def stream_message(self, request):  # pragma: no cover - not used
        if False:
            yield None


async def _capture_emit(events: list[StreamEvent], event: StreamEvent) -> None:
    events.append(event)


def _context() -> ToolExecutionContextService:
    return ToolExecutionContextService(cwd=Path("/tmp"))


async def test_tool_execution_context_service_unfolds_metadata_fields() -> None:
    context = ToolExecutionContextService(
        cwd="/tmp",
        services={"agent_name": "worker", "custom": "value"},
    )

    assert context.cwd == Path("/tmp")
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


async def test_hook_notification_uses_fallback_service_and_records_notification() -> None:
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
    assert result.metadata["system_notifications"][0]["text"] == "hook note"


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


async def test_query_loop_exposes_conversation_messages_to_prehooks() -> None:
    class _ConversationMessagesClient(SupportsStreamingMessages):
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
                                id="toolu_messages",
                                name="echo_tool",
                                input={"value": "seen"},
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
    tool.pre_hooks = (_ConversationMessagesPreHook(),)
    registry = ToolRegistry()
    registry.register(tool)
    context = QueryContext(
        api_client=_ConversationMessagesClient(),
        tool_registry=registry,
        cwd=Path("/tmp"),
        model="test",
        system_prompt="",
        max_tokens=100,
    )

    initial_messages = [ConversationMessage.from_user_text("start")]
    messages, stream = await run_query(context, initial_messages)
    async for _event, _usage in stream:
        pass

    assert [message.role for message in messages] == [
        "user",
        "assistant",
        "user",
        "assistant",
    ]
    assert tool.seen == ["seen:2"]


async def test_query_loop_emits_hook_notification_without_history_prompt() -> None:
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

    messages, stream = await run_query(context, [])
    events = []
    async for event, _usage in stream:
        events.append(event)

    assert any(
        isinstance(event, SystemNotification) and event.text == "hook note"
        for event in events
    )
    assert len(client.requests) == 2
    assert [message.role for message in messages] == ["assistant", "user", "assistant"]
    assert any(isinstance(block, ToolResultBlock) for block in messages[1].content)


async def test_query_loop_registers_run_notification_service_for_tool_body() -> None:
    class _ToolNotificationClient(SupportsStreamingMessages):
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
                                id="toolu_body_notify",
                                name="tool_notify",
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

    tool = _ToolNotifyTool()
    registry = ToolRegistry()
    registry.register(tool)
    client = _ToolNotificationClient()
    context = QueryContext(
        api_client=client,
        tool_registry=registry,
        cwd=Path("/tmp"),
        model="test",
        system_prompt="",
        max_tokens=100,
    )

    messages, stream = await run_query(context, [])
    events = []
    async for event, _usage in stream:
        events.append(event)

    assert tool.seen == ["hi"]
    assert any(
        isinstance(event, SystemNotification) and event.text == "tool note"
        for event in events
    )
    assert len(client.requests) == 2
    assert [message.role for message in messages] == ["assistant", "user", "assistant"]
    assert any(isinstance(block, ToolResultBlock) for block in messages[1].content)


async def test_query_loop_injects_opening_reminder_before_first_provider_request() -> None:
    class _OpeningReminderClient(SupportsStreamingMessages):
        def __init__(self) -> None:
            self.requests = []

        async def stream_message(self, request):
            self.requests.append(request)
            yield ApiMessageCompleteEvent(
                message=ConversationMessage(role="assistant", content=[]),
                usage=UsageSnapshot(),
            )

    client = _OpeningReminderClient()
    context = QueryContext(
        api_client=client,
        tool_registry=ToolRegistry(),
        cwd=Path("/tmp"),
        model="test",
        system_prompt="",
        max_tokens=100,
        notification_rules=[make_opening_reminder("follow the distilled rules")],
    )

    messages, stream = await run_query(
        context,
        [ConversationMessage.from_user_text("solve the task")],
    )
    async for _event, _usage in stream:
        pass

    assert len(client.requests) == 1
    first_request = client.requests[0]
    assert [message.role for message in first_request.messages] == ["user", "user"]
    reminder_block = first_request.messages[1].content[0]
    assert isinstance(reminder_block, SystemNotificationBlock)
    assert reminder_block.text == "follow the distilled rules"
    assert context.notification_fired == {"opening_reminder"}
    assert [message.role for message in messages] == ["user", "user", "assistant"]
    transcript_block = messages[1].content[0]
    assert isinstance(transcript_block, SystemNotificationBlock)
    assert transcript_block.text == "follow the distilled rules"


async def test_query_loop_injects_budget_warning_into_followup_provider_request() -> None:
    class _BudgetWarningClient(SupportsStreamingMessages):
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
                                id="toolu_echo",
                                name="echo_tool",
                                input={"value": "budgeted"},
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

    registry = ToolRegistry()
    registry.register(_EchoTool())
    client = _BudgetWarningClient()
    context = QueryContext(
        api_client=client,
        tool_registry=registry,
        cwd=Path("/tmp"),
        model="test",
        system_prompt="",
        max_tokens=100,
        tool_call_limit=2,
        notification_rules=[make_budget_warning(thresholds=(0.5,))],
    )

    messages, stream = await run_query(
        context,
        [ConversationMessage.from_user_text("solve the task")],
    )
    async for _event, _usage in stream:
        pass

    assert len(client.requests) == 2
    assert all(
        not isinstance(block, SystemNotificationBlock)
        for message in client.requests[0].messages
        for block in message.content
    )
    reminder_blocks = [
        block
        for message in client.requests[1].messages
        for block in message.content
        if isinstance(block, SystemNotificationBlock)
    ]
    assert len(reminder_blocks) == 1
    assert reminder_blocks[0].text.startswith("Tool-call budget at 50%")
    assert context.tool_calls_used == 1
    assert context.notification_state["budget_warning"]["last_fired"] == 0.5
    assert [message.role for message in messages] == ["user", "assistant", "user", "user", "assistant"]
    transcript_block = messages[3].content[0]
    assert isinstance(transcript_block, SystemNotificationBlock)
    assert transcript_block.text.startswith("Tool-call budget at 50%")


async def test_query_loop_runs_generic_context_preparers() -> None:
    class _Preparer:
        async def prepare_context_async(self, context: ToolExecutionContextService) -> None:
            context["prepared_by_test"] = "prepared"

    class _Client(SupportsStreamingMessages):
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
                                id="toolu_prepared",
                                name="prepared_context_tool",
                                input={"value": "ignored"},
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

    registry = ToolRegistry()
    registry.register(_PreparedContextTool())
    metadata = ExecutionMetadata(context_preparers=[_Preparer()])
    context = QueryContext(
        api_client=_Client(),
        tool_registry=registry,
        cwd=Path("/tmp"),
        model="test",
        system_prompt="",
        max_tokens=100,
        tool_metadata=metadata,
    )

    _messages, stream = await run_query(context, [])
    stream_events: list[StreamEvent] = []
    async for event, _usage in stream:
        stream_events.append(event)

    completed = [
        event
        for event in stream_events
        if isinstance(event, ToolExecutionCompleted)
    ]
    assert completed[-1].output == "prepared"
    assert context.tool_metadata is metadata
    assert context.tool_metadata.get("prepared_by_test") == "prepared"


async def test_query_loop_continues_after_non_terminal_tool_result() -> None:
    class _LoopClient(SupportsStreamingMessages):
        def __init__(self) -> None:
            self.requests = []

        async def stream_message(self, request):
            self.requests.append(request)
            if len(self.requests) == 1:
                yield ApiToolUseDeltaEvent(
                    id="toolu_echo",
                    name="echo_tool",
                    input={"value": "observed"},
                )
                yield ApiMessageCompleteEvent(
                    message=ConversationMessage(
                        role="assistant",
                        content=[
                            ToolUseBlock(
                                id="toolu_echo",
                                name="echo_tool",
                                input={"value": "observed"},
                            )
                        ],
                    ),
                    usage=UsageSnapshot(),
                )
            else:
                yield ApiMessageCompleteEvent(
                    message=ConversationMessage(
                        role="assistant",
                        content=[
                            ToolUseBlock(
                                id="toolu_term",
                                name="terminal_echo",
                                input={"value": "done"},
                            )
                        ],
                    ),
                    usage=UsageSnapshot(),
                )

    registry = ToolRegistry()
    registry.register(_EchoTool())
    registry.register(_TerminalEchoTool())
    client = _LoopClient()
    context = QueryContext(
        api_client=client,
        tool_registry=registry,
        cwd=Path("/tmp"),
        model="test",
        system_prompt="",
        max_tokens=100,
    )

    messages, stream = await run_query(context, [])
    async for _event, _usage in stream:
        pass

    assert len(client.requests) == 2
    assert [message.role for message in messages] == [
        "assistant",
        "user",
        "assistant",
    ]
    assert context.exit_reason is QueryExitReason.TOOL_STOP
    assert context.terminal_result is not None
    assert context.terminal_result.output == "done"


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


async def test_query_loop_captures_terminal_result_without_tool_result_prompt() -> None:
    class _TerminalClient(SupportsStreamingMessages):
        async def stream_message(self, request):
            yield ApiMessageCompleteEvent(
                message=ConversationMessage(
                    role="assistant",
                    content=[
                        ToolUseBlock(
                            id="toolu_term",
                            name="terminal_echo",
                            input={"value": "done"},
                        )
                    ],
                ),
                usage=UsageSnapshot(),
            )

    registry = ToolRegistry()
    registry.register(_TerminalEchoTool())
    context = QueryContext(
        api_client=_TerminalClient(),
        tool_registry=registry,
        cwd=Path("/tmp"),
        model="test",
        system_prompt="",
        max_tokens=100,
    )

    messages, stream = await run_query(context, [])
    async for _event, _usage in stream:
        pass

    assert context.exit_reason is QueryExitReason.TOOL_STOP
    assert context.terminal_result is not None
    assert context.terminal_result.output == "done"
    assert [message.role for message in messages] == ["assistant"]


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


async def test_background_dispatch_exposes_conversation_messages_to_prehooks() -> None:
    class _BackgroundEchoTool(_EchoTool):
        name = "background_echo"
        background = "optional"

    class _BackgroundConversationMessagesPreHook(_ConversationMessagesPreHook):
        target_tool = "background_echo"

    tool = _BackgroundEchoTool()
    tool.pre_hooks = (_BackgroundConversationMessagesPreHook(),)
    registry = ToolRegistry()
    registry.register(tool)
    context = QueryContext(
        api_client=_FakeClient(),
        tool_registry=registry,
        cwd=Path("/tmp"),
        model="test",
        system_prompt="",
        max_tokens=100,
    )
    manager = BackgroundTaskManager()
    tool_results: list[ToolResultBlock] = []
    conversation_messages = [
        ConversationMessage.from_user_text("start"),
        ConversationMessage(
            role="assistant",
            content=[
                ToolUseBlock(
                    id="toolu_bg",
                    name="background_echo",
                    input={"value": "seen", "background": True},
                )
            ],
        ),
    ]

    events = launch_and_collect_bg_events(
        context,
        conversation_messages,
        manager,
        ToolUseBlock(
            id="toolu_bg",
            name="background_echo",
            input={"value": "seen", "background": True},
        ),
        tool_results,
    )

    assert len(tool_results) == 1
    assert tool_results[0].is_error is False
    assert len(events) == 1

    completed = []
    for _ in range(20):
        await asyncio.sleep(0.01)
        completed = manager.collect_completed()
        if completed:
            break

    assert completed
    assert completed[0].result is not None
    assert completed[0].result.is_error is False
    assert completed[0].result.output == "seen:2"
    assert tool.seen == ["seen:2"]
