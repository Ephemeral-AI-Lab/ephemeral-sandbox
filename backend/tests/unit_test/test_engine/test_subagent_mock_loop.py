"""Mock query-loop coverage for typed subagent lifecycle surfaces."""

from __future__ import annotations

import asyncio
from collections.abc import AsyncGenerator, AsyncIterator, Callable, Iterator
from dataclasses import dataclass, field
from pathlib import Path
from types import SimpleNamespace
from typing import Any
from uuid import uuid4

import pytest
from pydantic import BaseModel

from agents import (
    AgentDefinition,
    AgentRole,
    AgentType,
    register_definition,
    unregister_definition,
)
from engine.agent.lifecycle import EphemeralRunResult
from engine.query.context import QueryContext
from engine.query.loop import run_query
from message.events import (
    AssistantMessageCompleteEvent,
    AssistantTextDeltaEvent,
    BackgroundTaskStartedEvent,
    StreamEvent,
    ThinkingDeltaEvent,
    ToolExecutionCompletedEvent,
    ToolUseDeltaEvent,
)
from message.message import (
    Message,
    TextBlock,
    ThinkingBlock,
    ToolResultBlock,
    ToolUseBlock,
)
from notification import SystemNotification
from providers.types import UsageSnapshot
from sandbox._shared.models import Intent
from tools._framework.core.base import (
    BaseTool,
    ExecutionMetadata,
    ToolExecutionContextService,
    ToolResult,
)
from tools._framework.core.registry import ToolRegistry
from tools.subagent.control import CancelSubagentTool, CheckSubagentProgressTool
from tools.subagent.run_subagent import run_subagent

pytestmark = pytest.mark.asyncio


@dataclass(frozen=True, slots=True)
class ToolCall:
    name: str
    input: dict = field(default_factory=dict)


@dataclass(frozen=True, slots=True)
class Turn:
    calls: tuple[ToolCall, ...] = ()
    thinking: str | None = None
    text: str | None = None


TurnScript = AsyncGenerator[Turn, list[ToolResultBlock]]


def _latest_tool_results(run_request: Any) -> list[ToolResultBlock]:
    for message in reversed(run_request.request.messages):
        if getattr(message, "role", None) != "user":
            continue
        blocks = [block for block in message.content if isinstance(block, ToolResultBlock)]
        if blocks:
            return blocks
    return []


class ScenarioEventSource:
    def __init__(self, script: TurnScript, *, agent_name: str = "") -> None:
        self._script = script
        self._agent_name = agent_name
        self._primed = False

    async def __call__(
        self,
        context: QueryContext,
        run_request: Any,
    ) -> AsyncIterator[StreamEvent]:
        del context
        turn = await self._advance(run_request)
        thinking_blocks = [ThinkingBlock(text=turn.thinking)] if turn.thinking else []
        text_blocks = [TextBlock(text=turn.text)] if turn.text else []
        tool_use_blocks = [
            ToolUseBlock(
                tool_use_id=f"toolu_{uuid4().hex}",
                name=call.name,
                input=dict(call.input),
            )
            for call in turn.calls
        ]
        if turn.thinking:
            yield ThinkingDeltaEvent(text=turn.thinking, agent_name=self._agent_name)
        if turn.text:
            yield AssistantTextDeltaEvent(text=turn.text, agent_name=self._agent_name)
        for block in tool_use_blocks:
            yield ToolUseDeltaEvent(
                tool_use_id=block.tool_use_id,
                name=block.name,
                input=block.input,
                agent_name=self._agent_name,
            )
        yield AssistantMessageCompleteEvent(
            message=Message(
                role="assistant",
                content=[*thinking_blocks, *text_blocks, *tool_use_blocks],
            ),
            usage=UsageSnapshot(),
            agent_name=self._agent_name,
        )

    async def _advance(self, run_request: Any) -> Turn:
        try:
            if not self._primed:
                self._primed = True
                return await self._script.asend(None)
            return await self._script.asend(_latest_tool_results(run_request))
        except StopAsyncIteration:
            return Turn()


class _UnusedClient:
    async def stream_message(self, _request: Any) -> AsyncIterator[StreamEvent]:
        raise AssertionError("ScenarioEventSource should bypass the API client")


class _SubmitDoneInput(BaseModel):
    summary: str


class _SubmitDoneTool(BaseTool):
    name = "submit_done"
    description = "Finish the parent run."
    input_model = _SubmitDoneInput
    intent = Intent.WRITE_ALLOWED
    is_terminal_tool = True

    async def execute(
        self,
        arguments: BaseModel,
        context: ToolExecutionContextService,
    ) -> ToolResult:
        del context
        assert isinstance(arguments, _SubmitDoneInput)
        return ToolResult(output=arguments.summary)


class _PauseInput(BaseModel):
    seconds: float = 0.01


class _PauseTool(BaseTool):
    name = "pause"
    description = "Yield the event loop in tests."
    input_model = _PauseInput
    intent = Intent.READ_ONLY

    async def execute(
        self,
        arguments: BaseModel,
        context: ToolExecutionContextService,
    ) -> ToolResult:
        del context
        assert isinstance(arguments, _PauseInput)
        await asyncio.sleep(arguments.seconds)
        return ToolResult(output="paused")


@pytest.fixture
def fake_subagent_name() -> Iterator[str]:
    name = "mock_loop_explorer"
    register_definition(
        AgentDefinition(
            name=name,
            description="Subagent used by mock-loop lifecycle tests.",
            agent_type=AgentType.SUBAGENT,
            role=AgentRole.SUBAGENT,
            terminals=["submit_exploration_result"],
            tool_call_limit=10,
        )
    )
    try:
        yield name
    finally:
        unregister_definition(name)


def _child_agent(*texts: str) -> SimpleNamespace:
    messages = [Message.from_user_text("child prompt")]
    messages.extend(
        Message(role="assistant", content=[TextBlock(text=text)])
        for text in texts
    )
    return SimpleNamespace(messages=messages)


def _terminal_result(output: str) -> EphemeralRunResult:
    return EphemeralRunResult(
        status="completed",
        error=None,
        terminal_result=ToolResult(output=output, is_terminal=True),
        agent_name="mock_loop_explorer",
        tool_call_count=1,
    )


def _no_terminal_result(agent_name: str) -> EphemeralRunResult:
    return EphemeralRunResult(
        status="completed",
        error=None,
        terminal_result=None,
        agent_name=agent_name,
        tool_call_count=1,
    )


def _context_for(script: Callable[[], TurnScript]) -> QueryContext:
    registry = ToolRegistry()
    registry.register(run_subagent)
    registry.register(CheckSubagentProgressTool())
    registry.register(CancelSubagentTool())
    registry.register(_SubmitDoneTool())
    registry.register(_PauseTool())

    metadata = ExecutionMetadata(
        runtime_config=SimpleNamespace(cwd="/tmp"),
        agent_name="parent-agent",
        agent_run_id="parent-run",
    )
    metadata["agent_type"] = AgentType.AGENT.value
    metadata["role"] = AgentRole.GENERATOR.value

    context = QueryContext(
        api_client=_UnusedClient(),
        tool_registry=registry,
        cwd=Path("/tmp"),
        model="mock-model",
        system_prompt="mock",
        max_tokens=4096,
        tool_call_limit=20,
        tool_metadata=metadata,
        enable_background_tasks=True,
        agent_name="parent-agent",
        agent_run_id="parent-run",
    )
    context.event_source = ScenarioEventSource(script(), agent_name="parent-agent")
    return context


async def _run_script(script: Callable[[], TurnScript]) -> tuple[QueryContext, list[StreamEvent]]:
    context = _context_for(script)
    messages, stream = await run_query(context, [Message.from_user_text("start")])
    events: list[StreamEvent] = []
    async for event, _usage in stream:
        events.append(event)
    return context, events


def _notifications(events: list[StreamEvent]) -> list[str]:
    return [event.text for event in events if isinstance(event, SystemNotification)]


def _completions(
    events: list[StreamEvent],
    *,
    tool_name: str | None = None,
) -> list[ToolExecutionCompletedEvent]:
    return [
        event
        for event in events
        if isinstance(event, ToolExecutionCompletedEvent)
        and (tool_name is None or event.tool_name == tool_name)
    ]


async def test_subagent_natural_completion_reaches_parent_notification(
    monkeypatch: pytest.MonkeyPatch,
    fake_subagent_name: str,
) -> None:
    async def _fake_runner(*_args: Any, **kwargs: Any) -> EphemeralRunResult:
        kwargs["on_agent_spawned"](_child_agent("natural findings ready"))
        await asyncio.sleep(0)
        return _terminal_result("natural findings delivered")

    monkeypatch.setattr("engine.api.run_ephemeral_agent", _fake_runner, raising=False)
    monkeypatch.setattr(
        "engine.agent.lifecycle.run_ephemeral_agent",
        _fake_runner,
        raising=False,
    )

    async def script() -> TurnScript:
        yield Turn(
            calls=(
                ToolCall(
                    "run_subagent",
                    {"agent_name": fake_subagent_name, "prompt": "explore"},
                ),
            )
        )
        yield Turn(calls=(ToolCall("pause", {}),))
        yield Turn(calls=(ToolCall("submit_done", {"summary": "parent done"}),))

    _context, events = await _run_script(script)

    assert any(isinstance(event, BackgroundTaskStartedEvent) for event in events)
    joined = "\n".join(_notifications(events))
    assert '[SUBAGENT COMPLETED] subagent_session_id="subagent_1" status=finished' in joined
    assert "natural findings delivered" in joined
    assert "bg_1" not in joined


async def test_subagent_no_terminal_failure_is_visible_to_parent_progress(
    monkeypatch: pytest.MonkeyPatch,
    fake_subagent_name: str,
) -> None:
    async def _fake_runner(*_args: Any, **kwargs: Any) -> EphemeralRunResult:
        kwargs["on_agent_spawned"](_child_agent("forgot to call terminal"))
        await asyncio.sleep(0)
        return _no_terminal_result(fake_subagent_name)

    monkeypatch.setattr("engine.api.run_ephemeral_agent", _fake_runner, raising=False)
    monkeypatch.setattr(
        "engine.agent.lifecycle.run_ephemeral_agent",
        _fake_runner,
        raising=False,
    )

    async def script() -> TurnScript:
        yield Turn(
            calls=(
                ToolCall(
                    "run_subagent",
                    {"agent_name": fake_subagent_name, "prompt": "explore"},
                ),
            )
        )
        yield Turn(calls=(ToolCall("pause", {}),))
        yield Turn(
            calls=(
                ToolCall(
                    "check_subagent_progress",
                    {"subagent_session_id": "subagent_1", "last_n_messages": 5},
                ),
            )
        )
        yield Turn(calls=(ToolCall("submit_done", {"summary": "parent done"}),))

    _context, events = await _run_script(script)

    progress = _completions(events, tool_name="check_subagent_progress")
    assert progress
    assert '"status": "failed"' in progress[-1].output
    assert "forgot to call terminal" in progress[-1].output
    joined = "\n".join(_notifications(events))
    assert '[SUBAGENT COMPLETED] subagent_session_id="subagent_1" status=failed' in joined
    assert "subagent exited without calling a terminal tool" in joined


async def test_cancel_subagent_reports_typed_cancelled_session(
    monkeypatch: pytest.MonkeyPatch,
    fake_subagent_name: str,
) -> None:
    cancelled = asyncio.Event()

    async def _fake_runner(*_args: Any, **kwargs: Any) -> EphemeralRunResult:
        kwargs["on_agent_spawned"](_child_agent("working before cancel"))
        try:
            await asyncio.Event().wait()
        except asyncio.CancelledError:
            cancelled.set()
            raise

    monkeypatch.setattr("engine.api.run_ephemeral_agent", _fake_runner, raising=False)
    monkeypatch.setattr(
        "engine.agent.lifecycle.run_ephemeral_agent",
        _fake_runner,
        raising=False,
    )

    async def script() -> TurnScript:
        yield Turn(
            calls=(
                ToolCall(
                    "run_subagent",
                    {"agent_name": fake_subagent_name, "prompt": "explore"},
                ),
            )
        )
        yield Turn(calls=(ToolCall("pause", {}),))
        yield Turn(
            calls=(
                ToolCall(
                    "cancel_subagent",
                    {
                        "subagent_session_id": "subagent_1",
                        "reason": "enough evidence",
                    },
                ),
            )
        )
        yield Turn(calls=(ToolCall("pause", {}),))
        yield Turn(
            calls=(
                ToolCall(
                    "check_subagent_progress",
                    {"subagent_session_id": "subagent_1", "last_n_messages": 5},
                ),
            )
        )
        yield Turn(calls=(ToolCall("pause", {}),))
        yield Turn(calls=(ToolCall("submit_done", {"summary": "parent done"}),))

    _context, events = await _run_script(script)

    assert cancelled.is_set()
    cancel_done = _completions(events, tool_name="cancel_subagent")
    assert cancel_done and "subagent_1" in cancel_done[-1].output
    progress = _completions(events, tool_name="check_subagent_progress")
    assert progress and '"status": "cancelled"' in progress[-1].output
    joined = "\n".join(_notifications(events))
    assert '[SUBAGENT COMPLETED] subagent_session_id="subagent_1" status=cancelled' in joined


async def test_parent_terminal_terminates_active_subagent_with_reason(
    monkeypatch: pytest.MonkeyPatch,
    fake_subagent_name: str,
) -> None:
    cancelled = asyncio.Event()
    audit_events: list[dict[str, Any]] = []

    async def _fake_runner(*_args: Any, **kwargs: Any) -> EphemeralRunResult:
        kwargs["on_agent_spawned"](_child_agent("still running"))
        try:
            await asyncio.Event().wait()
        except asyncio.CancelledError:
            cancelled.set()
            raise

    def _capture_audit(event: dict[str, Any], *, lane: str = "normal") -> None:
        del lane
        audit_events.append(event)

    monkeypatch.setattr("engine.api.run_ephemeral_agent", _fake_runner, raising=False)
    monkeypatch.setattr(
        "engine.agent.lifecycle.run_ephemeral_agent",
        _fake_runner,
        raising=False,
    )
    monkeypatch.setattr(
        "engine.background.task_supervisor.safe_emit",
        _capture_audit,
    )

    async def script() -> TurnScript:
        yield Turn(
            calls=(
                ToolCall(
                    "run_subagent",
                    {"agent_name": fake_subagent_name, "prompt": "keep working"},
                ),
            )
        )
        yield Turn(calls=(ToolCall("pause", {}),))
        yield Turn(calls=(ToolCall("submit_done", {"summary": "parent done"}),))

    context, events = await _run_script(script)

    assert context.terminal_result is not None
    assert cancelled.is_set()
    joined = "\n".join(_notifications(events))
    assert '[SUBAGENT COMPLETED] subagent_session_id="subagent_1" status=terminated' in joined
    assert "reason: non_cancellation_tool_request" in joined
    assert "bg_1" not in joined
    assert any(
        event.get("type") == "background_tool.cancelled"
        and event.get("payload", {})
        .get("background_tool", {})
        .get("cancel_reason")
        == "non_cancellation_tool_request"
        for event in audit_events
    )
