"""Loop-level coverage for the hard-ceiling termination model.

The single failure mode of ``_run_query_loop`` is reaching
``ceil(1.5 * tool_call_limit)`` tool calls without a terminal submission.
Terminal submissions short-circuit even when the ceiling has been crossed.
"""

from __future__ import annotations

import math
from collections.abc import AsyncIterator
from pathlib import Path
from typing import Any

import pytest

from engine.query.context import QueryContext, QueryExitReason
from engine.query.loop import _run_query_loop, terminal_submission_failed
from engine.tool_call.dispatch import AssistantToolDispatchOutcome
from message.events import (
    AssistantMessageCompleteEvent,
    StreamEvent,
    ToolExecutionCompletedEvent,
    ToolUseDeltaEvent,
)
from message.message import Message, ToolResultBlock, ToolUseBlock
from providers.types import MessageRequest, UsageSnapshot
from tools._framework.core.base import ExecutionMetadata
from tools._framework.core.registry import ToolRegistry
from tools._framework.core.results import ToolResult


class _ScriptedProvider:
    """One-turn fake that always emits a single read_file tool_use."""

    def __init__(self) -> None:
        self.calls: list[MessageRequest] = []

    async def stream_message(
        self, request: MessageRequest
    ) -> AsyncIterator[StreamEvent]:
        self.calls.append(request)
        msg = Message(
            role="assistant",
            content=[ToolUseBlock(tool_use_id="tu_1", name="read_file", input={})],
        )
        yield AssistantMessageCompleteEvent(message=msg, usage=UsageSnapshot())


def _build_context(*, used: int, limit: int) -> QueryContext:
    return QueryContext(
        api_client=_ScriptedProvider(),
        tool_registry=ToolRegistry(),
        cwd=Path("/tmp"),
        model="test-model",
        system_prompt="",
        max_tokens=32,
        tool_call_limit=limit,
        tool_calls_used=used,
        tool_metadata=ExecutionMetadata(),
        terminal_tools={"submit_x"},
    )


async def _drive_one_turn(
    context: QueryContext,
    messages: list[Message],
    *,
    dispatched_results: list[ToolResultBlock] | None = None,
    terminal_result: ToolResult | None = None,
    monkeypatch: pytest.MonkeyPatch,
) -> list[StreamEvent]:
    """Drive the loop for one full turn (provider stream + dispatch + exit)."""
    results = dispatched_results or [
        ToolResultBlock(tool_use_id="tu_1", content="ok", is_error=False),
    ]

    async def _fake_dispatch(*_args: Any, **_kwargs: Any) -> AssistantToolDispatchOutcome:
        return AssistantToolDispatchOutcome(
            tool_results=results,
            terminal_result=terminal_result,
        )

    monkeypatch.setattr("engine.query.loop.dispatch_assistant_tools", _fake_dispatch)

    events: list[StreamEvent] = []
    async for event, _usage in _run_query_loop(context, messages):
        events.append(event)
    return events


# ----------------------------------------------------------------------------
# Numeric predicate
# ----------------------------------------------------------------------------


def test_terminal_submission_failed_below_ceiling() -> None:
    ctx = _build_context(used=math.ceil(1.5 * 10) - 1, limit=10)
    assert terminal_submission_failed(ctx) is False


def test_terminal_submission_failed_at_ceiling() -> None:
    ctx = _build_context(used=math.ceil(1.5 * 10), limit=10)
    assert terminal_submission_failed(ctx) is True


def test_terminal_submission_failed_above_ceiling() -> None:
    ctx = _build_context(used=math.ceil(1.5 * 10) + 5, limit=10)
    assert terminal_submission_failed(ctx) is True


# ----------------------------------------------------------------------------
# Loop integration
# ----------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_loop_exits_when_streamed_dispatch_bumps_counter_to_ceiling(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Streamed ``ToolUseDeltaEvent`` increments the counter; one stream
    tipping ``used`` to the ceiling triggers ``TERMINAL_NOT_SUBMITTED``."""
    limit = 10

    class _StreamingProvider:
        async def stream_message(self, request):  # noqa: ARG002
            yield ToolUseDeltaEvent(
                tool_use_id="tu_1", name="read_file", input={}
            )
            yield AssistantMessageCompleteEvent(
                message=Message(
                    role="assistant",
                    content=[
                        ToolUseBlock(tool_use_id="tu_1", name="read_file", input={})
                    ],
                ),
                usage=UsageSnapshot(),
            )

    context = QueryContext(
        api_client=_StreamingProvider(),
        tool_registry=ToolRegistry(),
        cwd=Path("/tmp"),
        model="test-model",
        system_prompt="",
        max_tokens=32,
        tool_call_limit=limit,
        # One dispatch this turn bumps used to ceiling.
        tool_calls_used=math.ceil(1.5 * limit) - 1,
        tool_metadata=ExecutionMetadata(),
        terminal_tools={"submit_x"},
    )

    messages: list[Message] = [Message.from_user_text("go")]
    await _drive_one_turn(context, messages, monkeypatch=monkeypatch)

    assert context.exit_reason == QueryExitReason.TERMINAL_NOT_SUBMITTED
    assert context.tool_calls_used == math.ceil(1.5 * limit)


@pytest.mark.asyncio
async def test_loop_exits_terminal_not_submitted_when_ceiling_crossed(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    limit = 4
    # Already at the ceiling before this turn even starts.
    context = _build_context(used=math.ceil(1.5 * limit), limit=limit)
    messages: list[Message] = [Message.from_user_text("go")]

    events = await _drive_one_turn(context, messages, monkeypatch=monkeypatch)

    assert context.exit_reason == QueryExitReason.TERMINAL_NOT_SUBMITTED
    # The synthetic error event is on the stream.
    error_events = [
        e for e in events
        if isinstance(e, ToolExecutionCompletedEvent) and e.is_error
    ]
    assert error_events
    assert "terminal tool not submitted" in error_events[-1].output


@pytest.mark.asyncio
async def test_text_only_turns_also_reach_terminal_not_submitted(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    limit = 1

    class _TextOnlyProvider:
        def __init__(self) -> None:
            self.calls = 0

        async def stream_message(self, request):  # noqa: ARG002
            self.calls += 1
            if self.calls > 2:
                raise AssertionError("loop requested an unexpected third turn")
            yield AssistantMessageCompleteEvent(
                message=Message(
                    role="assistant",
                    content=[],
                ),
                usage=UsageSnapshot(),
            )

    provider = _TextOnlyProvider()
    context = QueryContext(
        api_client=provider,
        tool_registry=ToolRegistry(),
        cwd=Path("/tmp"),
        model="test-model",
        system_prompt="",
        max_tokens=32,
        tool_call_limit=limit,
        tool_calls_used=0,
        tool_metadata=ExecutionMetadata(),
        terminal_tools={"submit_x"},
    )
    messages: list[Message] = [Message.from_user_text("go")]

    async def _unexpected_dispatch(*_args: Any, **_kwargs: Any) -> AssistantToolDispatchOutcome:
        raise AssertionError("text-only turns must not dispatch tools")

    monkeypatch.setattr("engine.query.loop.dispatch_assistant_tools", _unexpected_dispatch)

    events: list[StreamEvent] = []
    async for event, _usage in _run_query_loop(context, messages):
        events.append(event)

    assert provider.calls == 2
    assert context.exit_reason == QueryExitReason.TERMINAL_NOT_SUBMITTED
    assert context.text_only_no_terminal_turns == 2
    error_events = [
        e for e in events
        if isinstance(e, ToolExecutionCompletedEvent) and e.is_error
    ]
    assert error_events
    assert "terminal tool not submitted" in error_events[-1].output


@pytest.mark.asyncio
async def test_terminal_result_short_circuits_ceiling(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """A successful terminal submission wins even past the ceiling."""
    limit = 4
    context = _build_context(used=math.ceil(1.5 * limit) + 100, limit=limit)
    messages: list[Message] = [Message.from_user_text("go")]
    terminal_result = ToolResult(
        output="delivered", is_error=False, is_terminal=True
    )

    await _drive_one_turn(
        context,
        messages,
        terminal_result=terminal_result,
        dispatched_results=[
            ToolResultBlock(
                tool_use_id="tu_1",
                content="delivered",
                is_error=False,
                is_terminal=True,
            )
        ],
        monkeypatch=monkeypatch,
    )

    assert context.exit_reason == QueryExitReason.TOOL_STOP
    assert context.terminal_result is not None


@pytest.mark.asyncio
async def test_loop_cancels_background_tasks_on_hard_exit(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Hard-ceiling exit must call ``cancel_all`` on the background supervisor."""
    limit = 4
    context = _build_context(used=math.ceil(1.5 * limit), limit=limit)
    context.enable_background_tasks = True
    messages: list[Message] = [Message.from_user_text("go")]

    cancel_calls: list[str] = []

    async def _record_cancel(self: Any) -> None:  # noqa: ARG001
        cancel_calls.append("cancel_all")

    monkeypatch.setattr(
        "engine.background.task_supervisor.BackgroundTaskSupervisor.cancel_all",
        _record_cancel,
    )
    # has_pending is consulted in the finally block; return False so we
    # observe only the explicit cancel from the hard-exit branch.
    monkeypatch.setattr(
        "engine.background.task_supervisor.BackgroundTaskSupervisor.has_pending",
        lambda self: False,
    )

    await _drive_one_turn(context, messages, monkeypatch=monkeypatch)

    assert context.exit_reason == QueryExitReason.TERMINAL_NOT_SUBMITTED
    assert cancel_calls == ["cancel_all"]
