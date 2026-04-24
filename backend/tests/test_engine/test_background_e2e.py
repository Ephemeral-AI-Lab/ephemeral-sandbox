# ruff: noqa
"""E2E tests for background task execution through the query loop.

Tests the full background task lifecycle using scripted mock LLM responses
to validate that the engine correctly handles:
1. LLM deciding to background a tool vs foreground
2. LLM doing foreground work while background runs, then going idle
3. LLM proactively calling check_background_progress
4. LLM cancelling a background task after seeing failures
5. LLM cancelling a hanging background task after repeated checks

Uses a mock LLM client with scripted responses and a fake slow tool
to simulate real background execution scenarios without hitting real APIs.
"""

from __future__ import annotations

import asyncio
import logging
from dataclasses import dataclass
from pathlib import Path
from types import SimpleNamespace
from typing import Any, AsyncIterator

import pytest

from agents import get_definition as _get_agent_def
from engine.runtime.background_tasks import BackgroundTaskManager
from message import ConversationMessage, TextBlock, ToolResultBlock, ToolUseBlock
from engine.core.query import QueryContext, _run_query_loop
from message.stream_events import (
    AssistantTextDelta,
    AssistantTurnComplete,
    BackgroundTaskCompleted,
    BackgroundTaskStarted,
    StreamEvent,
    ToolExecutionCompleted,
    ToolExecutionStarted,
)
from providers.types import (
    ApiMessageCompleteEvent,
    ApiStreamEvent,
    ApiTextDeltaEvent,
    ApiToolUseDeltaEvent,
    UsageSnapshot,
)
from tools.builtins.background.check_background_progress import (
    CheckBackgroundProgressInput,
    CheckBackgroundProgressTool,
)
from tools.core.base import BaseTool, ToolExecutionContext, ToolRegistry, ToolResult
from tools.core.runtime import ExecutionMetadata
from tools.subagent.run_subagent_tool import run_subagent
from pydantic import BaseModel, Field

logger = logging.getLogger(__name__)
logging.basicConfig(level=logging.DEBUG, format="%(asctime)s [%(name)s] %(levelname)s: %(message)s")

pytestmark = pytest.mark.e2e

if _get_agent_def("scout") is None:
    from team.definitions import register_all as _register_team_builtins

    try:
        _register_team_builtins()
    except Exception:
        pass


# ---------------------------------------------------------------------------
# Fake slow tool — simulates daytona_shell with configurable delay and output
# ---------------------------------------------------------------------------


class SlowToolInput(BaseModel):
    """Input for the fake slow tool."""

    command: str = Field(description="Command to simulate")
    delay: float = Field(default=0.1, description="Seconds to sleep")


class SlowTool(BaseTool):
    """A fake tool that sleeps, then returns output. Supports background."""

    name: str = "fake_bash"
    description: str = "Run a fake shell command with configurable delay."
    input_model: type[BaseModel] = SlowToolInput
    background = "optional"

    def __init__(self, output: str = "command completed", is_error: bool = False) -> None:
        self._output = output
        self._is_error = is_error

    async def execute(self, arguments: BaseModel, context: ToolExecutionContext) -> ToolResult:
        assert isinstance(arguments, SlowToolInput)
        logger.info(f"[SlowTool] Executing: {arguments.command} (delay={arguments.delay}s)")
        await asyncio.sleep(arguments.delay)
        logger.info(f"[SlowTool] Done: {arguments.command} -> {self._output[:100]}")
        return ToolResult(output=self._output, is_error=self._is_error)


class FastToolInput(BaseModel):
    """Input for the fake fast tool."""

    action: str = Field(description="Action to perform")


class FastTool(BaseTool):
    """A fake fast tool that returns immediately. Does NOT support background."""

    name: str = "fake_edit"
    description: str = "A fast tool that completes immediately."
    input_model: type[BaseModel] = FastToolInput
    background = "forbidden"

    async def execute(self, arguments: BaseModel, context: ToolExecutionContext) -> ToolResult:
        assert isinstance(arguments, FastToolInput)
        logger.info(f"[FastTool] Executing: {arguments.action}")
        return ToolResult(output=f"Edited: {arguments.action}", is_error=False)


# ---------------------------------------------------------------------------
# Scripted mock LLM client
# ---------------------------------------------------------------------------


class ScriptedMockClient:
    """Mock LLM client that returns scripted responses in sequence.

    Each call to stream_message returns the next response in the script.
    Captures all requests for assertion. Logs each turn for debugging.
    """

    def __init__(self, responses: list[ConversationMessage]) -> None:
        self.responses = responses
        self._call_count = 0
        self.all_requests: list[Any] = []

    async def stream_message(self, request: Any) -> AsyncIterator[ApiStreamEvent]:
        self.all_requests.append(request)
        idx = min(self._call_count, len(self.responses) - 1)
        msg = self.responses[idx]
        self._call_count += 1

        logger.info(
            f"[MockLLM] Turn {self._call_count}: "
            f"text={msg.text[:80]!r}, "
            f"tool_uses={[tu.name for tu in msg.tool_uses]}"
        )

        # Stream text deltas
        for block in msg.content:
            if isinstance(block, TextBlock):
                yield ApiTextDeltaEvent(text=block.text)

        yield ApiMessageCompleteEvent(
            message=msg,
            usage=UsageSnapshot(input_tokens=100, output_tokens=50),
            stop_reason="end_turn",
        )


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _make_registry(*tools: BaseTool) -> ToolRegistry:
    """Create a ToolRegistry with given tools."""
    registry = ToolRegistry()
    for t in tools:
        registry.register(t)
    return registry


def _make_context(
    client: ScriptedMockClient,
    registry: ToolRegistry,
    enable_background: bool = True,
) -> QueryContext:
    """Create a QueryContext for testing."""
    return QueryContext(
        api_client=client,
        tool_registry=registry,
        cwd=Path("/tmp/test"),
        model="test-model",
        system_prompt="You are a test assistant.",
        max_tokens=4096,
        enable_background_tasks=enable_background,
    )


async def _collect_events(
    context: QueryContext, messages: list[ConversationMessage]
) -> list[StreamEvent]:
    """Run the query loop and collect all events."""
    events: list[StreamEvent] = []
    async for event, _usage in _run_query_loop(context, messages):
        event_type = type(event).__name__
        logger.debug(f"[Event] {event_type}: {str(event)[:200]}")
        events.append(event)
    return events


def _events_of_type(events: list[StreamEvent], cls: type) -> list:
    """Filter events by type."""
    return [e for e in events if isinstance(e, cls)]


def _msg_text(text: str) -> ConversationMessage:
    """Create an assistant text-only message (no tool calls → loop stops)."""
    return ConversationMessage(role="assistant", content=[TextBlock(text=text)])


def _msg_tool(tool_name: str, tool_input: dict, text: str = "") -> ConversationMessage:
    """Create an assistant message with a tool call."""
    content: list = []
    if text:
        content.append(TextBlock(text=text))
    content.append(ToolUseBlock(name=tool_name, input=tool_input))
    return ConversationMessage(role="assistant", content=content)


def _msg_tools(*tool_calls: tuple[str, dict], text: str = "") -> ConversationMessage:
    """Create an assistant message with multiple tool calls."""
    content: list = []
    if text:
        content.append(TextBlock(text=text))
    for name, inp in tool_calls:
        content.append(ToolUseBlock(name=name, input=inp))
    return ConversationMessage(role="assistant", content=content)


# ===========================================================================
# Test 1: LLM decides to background vs foreground
# ===========================================================================


class TestLLMDecidesToBackground:
    """LLM sends background=true for slow tool, foreground for fast tool.
    Validates that background="optional" is required and LLM has choice.
    """

    async def test_llm_chooses_background_for_slow_tool(self):
        """LLM sends background=true → engine launches async, returns immediately."""
        slow_tool = SlowTool(output="tests passed: 48/48")
        registry = _make_registry(slow_tool)

        client = ScriptedMockClient(
            [
                # Turn 1: LLM decides to background the slow command
                _msg_tool(
                    "fake_bash",
                    {"command": "pytest", "delay": 0.5, "background": True},
                    text="Running tests in background...",
                ),
                # Turn 2: LLM has no more work (idle → wait for background)
                _msg_text("Waiting for tests to complete."),
                # Turn 3: After background completes, LLM reacts
                _msg_text("All 48 tests passed!"),
            ]
        )

        context = _make_context(client, registry)
        messages = [ConversationMessage.from_user_text("Run the tests")]
        events = await _collect_events(context, messages)

        # Should have BackgroundTaskStarted
        bg_started = _events_of_type(events, BackgroundTaskStarted)
        assert len(bg_started) == 1, f"Expected 1 BackgroundTaskStarted, got {len(bg_started)}"
        assert bg_started[0].tool_name == "fake_bash"
        logger.info(f"[PASS] Background task started: {bg_started[0].task_id}")

        # Should have BackgroundTaskCompleted
        bg_completed = _events_of_type(events, BackgroundTaskCompleted)
        assert len(bg_completed) == 1, (
            f"Expected 1 BackgroundTaskCompleted, got {len(bg_completed)}"
        )
        assert "tests passed" in bg_completed[0].output
        logger.info(f"[PASS] Background task completed with output: {bg_completed[0].output[:100]}")

    async def test_llm_chooses_foreground_for_same_tool(self):
        """LLM does NOT send background=true → tool runs in foreground (blocking)."""
        slow_tool = SlowTool(output="quick result")
        registry = _make_registry(slow_tool)

        client = ScriptedMockClient(
            [
                # Turn 1: LLM runs the tool in foreground (no background flag)
                _msg_tool("fake_bash", {"command": "echo hello", "delay": 0.01}),
                # Turn 2: LLM done
                _msg_text("Done."),
            ]
        )

        context = _make_context(client, registry)
        messages = [ConversationMessage.from_user_text("Run a quick command")]
        events = await _collect_events(context, messages)

        # Should NOT have BackgroundTaskStarted
        bg_started = _events_of_type(events, BackgroundTaskStarted)
        assert len(bg_started) == 0, f"Expected no BackgroundTaskStarted, got {len(bg_started)}"

        # Should have normal ToolExecutionCompleted
        tool_completed = _events_of_type(events, ToolExecutionCompleted)
        assert len(tool_completed) == 1
        assert "quick result" in tool_completed[0].output
        logger.info("[PASS] Tool ran in foreground as expected")

    async def test_background_rejected_for_unsupported_tool(self):
        """LLM sends background=true on a tool that doesn't support it → error."""
        fast_tool = FastTool()
        registry = _make_registry(fast_tool)

        client = ScriptedMockClient(
            [
                # Turn 1: LLM tries to background a fast tool
                _msg_tool("fake_edit", {"action": "fix config", "background": True}),
                # Turn 2: LLM sees error, adapts
                _msg_text("I see the tool doesn't support background. Let me run it normally."),
            ]
        )

        context = _make_context(client, registry)
        messages = [ConversationMessage.from_user_text("Fix the config")]
        events = await _collect_events(context, messages)

        # Should NOT have BackgroundTaskStarted
        bg_started = _events_of_type(events, BackgroundTaskStarted)
        assert len(bg_started) == 0

        # Should have error in tool completion
        tool_completed = _events_of_type(events, ToolExecutionCompleted)
        assert any("does not support background" in tc.output for tc in tool_completed), (
            f"Expected rejection message. Got: {[tc.output for tc in tool_completed]}"
        )
        logger.info("[PASS] Background correctly rejected for unsupported tool")

    async def test_run_subagent_preflight_does_not_hard_reject_without_background_launch(
        self, monkeypatch
    ):
        class _StubConfig:
            cwd = Path("/tmp")
            session_id = "S1"

        registry = _make_registry(run_subagent)
        client = ScriptedMockClient(
            [
                _msg_tool(
                    "run_subagent",
                    {"agent_name": "scout", "input": {"target_paths": ["pkg/core.py"]}},
                    text="Opening the scout lane.",
                ),
                _msg_text("Need to anchor scope first."),
            ]
        )

        context = _make_context(client, registry)
        context.tool_metadata = ExecutionMetadata(
            session_config=_StubConfig(),
            agent_name="team_planner",
            team_run_id="TR_BENCH",
            work_item_id="ROOT",
        )
        team_run = SimpleNamespace(
            root_task_id="ROOT",
            task_center=SimpleNamespace(
                graph={
                    "ROOT": SimpleNamespace(
                        payload={"fail_to_pass": ["pkg/tests/test_api.py::test_one"]}
                    )
                }
            ),
        )
        monkeypatch.setattr(
            "team.runtime.run_registry.get",
            lambda team_run_id: team_run if team_run_id == "TR_BENCH" else None,
        )

        events = await _collect_events(
            context,
            [ConversationMessage.from_user_text("Plan the benchmark run")],
        )

        bg_started = _events_of_type(events, BackgroundTaskStarted)
        assert len(bg_started) == 1, f"Expected 1 BackgroundTaskStarted, got {len(bg_started)}"
        assert bg_started[0].tool_name == "run_subagent"

        tool_completed = _events_of_type(events, ToolExecutionCompleted)
        assert not any(tc.tool_name == "run_subagent" for tc in tool_completed), (
            f"Expected no synchronous hard rejection. Got: {[tc.output for tc in tool_completed]}"
        )


# ===========================================================================
# Test 2: Foreground work while background runs, then idle notification
# ===========================================================================


class TestForegroundWhileBackgroundRuns:
    """LLM does foreground work while a background task runs.
    When foreground work finishes, engine idle-waits and injects result.
    """

    async def test_foreground_then_idle_wait(self):
        """LLM backgrounds slow task, does foreground work, goes idle, gets result."""
        slow_tool = SlowTool(output="BUILD SUCCESSFUL in 2s")
        fast_tool = FastTool()
        registry = _make_registry(slow_tool, fast_tool)

        client = ScriptedMockClient(
            [
                # Turn 1: LLM backgrounds the build AND does a foreground edit
                _msg_tools(
                    ("fake_bash", {"command": "npm run build", "delay": 0.5, "background": True}),
                    ("fake_edit", {"action": "fix typo in readme"}),
                    text="Building in background while I fix the readme...",
                ),
                # Turn 2: LLM finishes foreground, goes idle (no tool calls)
                _msg_text("README fixed. Waiting for build..."),
                # Turn 3: Engine injected background result → LLM reacts
                _msg_text("Build succeeded! All done."),
            ]
        )

        context = _make_context(client, registry)
        messages = [ConversationMessage.from_user_text("Build the project and fix the readme")]
        events = await _collect_events(context, messages)

        # Verify background started
        bg_started = _events_of_type(events, BackgroundTaskStarted)
        assert len(bg_started) == 1
        assert bg_started[0].tool_name == "fake_bash"

        # Verify foreground tool completed normally
        tool_completed = _events_of_type(events, ToolExecutionCompleted)
        foreground_results = [tc for tc in tool_completed if "Edited:" in tc.output]
        assert len(foreground_results) >= 1, "Foreground edit should complete"

        # Verify background completed
        bg_completed = _events_of_type(events, BackgroundTaskCompleted)
        assert len(bg_completed) == 1
        assert "BUILD SUCCESSFUL" in bg_completed[0].output

        # Verify LLM got 3 turns
        turns = _events_of_type(events, AssistantTurnComplete)
        assert len(turns) == 3, f"Expected 3 LLM turns, got {len(turns)}"
        logger.info(
            "[PASS] Foreground work completed while background ran, idle wait delivered result"
        )


# ===========================================================================
# Test 6: Mixed scenario — background + foreground + progress + completion
# ===========================================================================


class TestFullBackgroundLifecycle:
    """Complete lifecycle: background launch → foreground work → progress check → completion."""

    async def test_complete_lifecycle(self):
        """Full lifecycle with all components working together."""
        slow_tool = SlowTool(output="BUILD OK\n48/48 tests passed\n0 failures")
        fast_tool = FastTool()
        registry = _make_registry(slow_tool, fast_tool)

        client = ScriptedMockClient(
            [
                # Turn 1: Background build + foreground edit
                _msg_tools(
                    (
                        "fake_bash",
                        {"command": "npm run build && npm test", "delay": 0.3, "background": True},
                    ),
                    ("fake_edit", {"action": "update version to 2.0"}),
                    text="Building and testing in background. Updating version...",
                ),
                # Turn 2: Another foreground task
                _msg_tool(
                    "fake_edit", {"action": "update changelog"}, text="Also updating the changelog."
                ),
                # Turn 3: Check progress
                _msg_tool(
                    "check_background_progress", {"task_id": "all"}, text="Checking build status..."
                ),
                # Turn 4: Go idle — engine will wait and inject result
                _msg_text("Build should be done soon, waiting..."),
                # Turn 5: React to completion
                _msg_text("Build succeeded, all 48 tests passed. Version 2.0 is ready!"),
            ]
        )

        context = _make_context(client, registry)
        messages = [ConversationMessage.from_user_text("Release version 2.0")]
        events = await _collect_events(context, messages)

        # Full lifecycle checks
        bg_started = _events_of_type(events, BackgroundTaskStarted)
        assert len(bg_started) == 1, "One background task should start"

        fg_completed = [
            tc
            for tc in _events_of_type(events, ToolExecutionCompleted)
            if tc.tool_name == "fake_edit"
        ]
        assert len(fg_completed) >= 2, "Two foreground edits should complete"

        progress = [
            tc
            for tc in _events_of_type(events, ToolExecutionCompleted)
            if tc.tool_name == "check_background_progress"
        ]
        assert len(progress) >= 1, "At least one progress check"

        bg_completed = _events_of_type(events, BackgroundTaskCompleted)
        assert len(bg_completed) == 1, "Background should complete"
        assert "BUILD OK" in bg_completed[0].output

        turns = _events_of_type(events, AssistantTurnComplete)
        assert len(turns) == 5, f"Expected 5 turns, got {len(turns)}"

        logger.info(
            f"[PASS] Full lifecycle: {len(bg_started)} bg started, "
            f"{len(fg_completed)} fg completed, "
            f"{len(progress)} progress checks, "
            f"{len(bg_completed)} bg completed, "
            f"{len(turns)} total turns"
        )


# ===========================================================================
# Test: live progress tail surfaced through check_background_progress
# ===========================================================================


class StreamingToolInput(BaseModel):
    n_lines: int = Field(default=5)
    interval: float = Field(default=0.05)


class StreamingTool(BaseTool):
    """Background-capable tool that emits progress lines via on_progress_line.

    Mirrors how a real streaming-capable tool (e.g. a sandbox shell with
    incremental stdout) would push lines into the BackgroundTaskManager
    while still running.
    """

    name: str = "fake_streaming"
    description: str = "Emit n_lines progress lines, sleeping interval between each."
    input_model: type[BaseModel] = StreamingToolInput
    background = "optional"

    async def execute(self, arguments: BaseModel, context: ToolExecutionContext) -> ToolResult:
        assert isinstance(arguments, StreamingToolInput)
        on_line = context.metadata.get("on_progress_line")
        for i in range(arguments.n_lines):
            if on_line is not None:
                on_line(f"line {i + 1}")
            await asyncio.sleep(arguments.interval)
        return ToolResult(output="\n".join(f"line {i + 1}" for i in range(arguments.n_lines)))


class TestLiveProgressTail:
    """Verifies check_background_progress returns a live tail of streamed
    output while the underlying background task is still running."""

    async def test_live_tail_visible_while_running(self) -> None:
        """While the streaming tool is mid-flight, check_background_progress
        must return the lines already emitted via on_progress_line, with
        last_n_lines honoured. After completion, the final output must be
        available."""
        from engine.runtime.background_tasks import BackgroundTaskManager

        mgr = BackgroundTaskManager()
        tool = StreamingTool()

        n_lines = 6
        interval = 0.08
        alias = mgr.next_alias()

        # Wrap the tool the way query.py does for background launches:
        # the manager-bound on_progress_line callback is injected via
        # ToolExecutionContext.metadata so the tool can stream into the
        # tracked task without knowing about the manager.
        async def _coro() -> ToolResult:
            ctx = ToolExecutionContext(
                cwd=Path("/tmp"),
                metadata={"on_progress_line": mgr.make_progress_callback(alias)},
            )
            return await tool.execute(StreamingToolInput(n_lines=n_lines, interval=interval), ctx)

        mgr.launch(alias, "fake_streaming", {}, _coro())

        # Wait long enough for ~3 lines to have been emitted, but not all 6.
        await asyncio.sleep(interval * 3 + interval / 2)

        check_tool = CheckBackgroundProgressTool()
        check_ctx = ToolExecutionContext(
            cwd=Path("/tmp"),
            metadata={"background_task_manager": mgr},
        )

        mid_result = await check_tool.execute(
            CheckBackgroundProgressInput(task_id=alias, last_n_lines=2),
            check_ctx,
        )
        assert not mid_result.is_error, mid_result.output
        # Status snapshot should report running, with a tail of streamed lines.
        assert '"status": "running"' in mid_result.output, mid_result.output
        assert '"output"' in mid_result.output, (
            f"Expected live tail in mid-flight check, got:\n{mid_result.output}"
        )
        # last_n_lines=2 → only the most recent two streamed lines should
        # appear, and earlier ones should NOT.
        assert "line 1" not in mid_result.output, mid_result.output
        # At least one of the recent lines must be present.
        assert any(f"line {i}" in mid_result.output for i in (2, 3, 4)), mid_result.output

        # Now wait for completion and re-check.
        completed = await mgr.wait_for(alias, timeout=5.0)
        assert completed is not None, "task should complete within timeout"
        assert completed.status in ("completed", "delivered")

        final_result = await check_tool.execute(
            CheckBackgroundProgressInput(task_id=alias, last_n_lines=20),
            check_ctx,
        )
        assert not final_result.is_error
        assert '"status":' in final_result.output
        assert "completed" in final_result.output or "delivered" in final_result.output
        assert f"line {n_lines}" in final_result.output

    async def test_running_task_shows_start_stamp_not_final_output(self) -> None:
        """A non-streaming background task should surface only the
        ``[started: ...]`` stamp while running — not the final output that
        the inner coroutine will eventually return. Final output is only
        revealed after completion."""
        from engine.runtime.background_tasks import BackgroundTaskManager

        mgr = BackgroundTaskManager()

        async def _coro() -> ToolResult:
            await asyncio.sleep(0.3)
            return ToolResult(output="final only")

        alias = mgr.next_alias()
        mgr.launch(alias, "noop", {}, _coro())

        await asyncio.sleep(0.05)
        snap = mgr.get_status(alias)
        assert snap and snap[0]["status"] == "running"
        running_output = snap[0].get("output", "")
        assert running_output.startswith("[started:"), running_output
        assert "final only" not in running_output, (
            f"Non-streaming task must not leak final output mid-run: {snap[0]}"
        )

        await mgr.wait_for(alias, timeout=2.0)
        snap = mgr.get_status(alias)
        assert snap[0]["status"] in ("completed", "delivered")
        assert snap[0]["output"] == "final only"
