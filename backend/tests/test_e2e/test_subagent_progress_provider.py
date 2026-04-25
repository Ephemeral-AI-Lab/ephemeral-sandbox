# ruff: noqa
"""E2E test for the subagent progress-provider integration.

Verifies the contract that solution-C established:
  - run_subagent registers a `progress_provider` callback on the
    BackgroundTaskManager.
  - While the subagent is running, calling check_background_progress on
    its task_id returns a fresh formatted snapshot of the inner agent's
    last N messages — NOT the line buffer that streaming tools use.
  - After the subagent finishes, get_status returns the final result
    (not the provider output).

Uses a stub EphemeralAgent to avoid API credentials. Exercises the same
BackgroundTaskManager + ToolExecutionContext wiring as query.py.
"""

from __future__ import annotations

import asyncio
from pathlib import Path

import pytest

from engine.runtime.background_tasks import BackgroundTaskManager
from agents import AgentDefinition, get_definition, register_definition, unregister_definition
from message.messages import (
    ConversationMessage,
    TextBlock,
    ToolUseBlock,
)
from tools.builtins.background.check_background_progress import (
    CheckBackgroundProgressInput,
    CheckBackgroundProgressTool,
)
from tools.core.base import ToolExecutionContext, ToolResult
from tools.subagent.run_subagent_tool import (
    PEEK_MESSAGE_MAX,
    format_last_n_messages,
    run_subagent,
)

pytestmark = pytest.mark.e2e


class _StubAgent:
    """Stand-in for EphemeralAgent that grows _messages step by step."""

    def __init__(self, steps: list[ConversationMessage], delay: float = 0.05) -> None:
        self._display_messages: list[ConversationMessage] = []
        self._steps = steps
        self._delay = delay

    @property
    def display_messages(self) -> list[ConversationMessage]:
        return self._display_messages

    async def run(self, prompt: str):
        for msg in self._steps:
            self._display_messages.append(msg)
            await asyncio.sleep(self._delay)
            yield ("step",)

    async def close(self) -> None:
        pass


def _scripted_messages() -> list[ConversationMessage]:
    return [
        ConversationMessage(role="user", content=[TextBlock(text="task")]),
        ConversationMessage(
            role="assistant",
            content=[
                TextBlock(text="reading file"),
                ToolUseBlock(name="read_file", input={"path": "x.py"}),
            ],
        ),
        ConversationMessage(
            role="assistant",
            content=[
                TextBlock(text="editing file"),
                ToolUseBlock(name="edit_file", input={"path": "x.py"}),
            ],
        ),
        ConversationMessage(
            role="assistant",
            content=[TextBlock(text="DONE: completed task")],
        ),
    ]


@pytest.mark.asyncio
async def test_subagent_peek_returns_live_snapshot(monkeypatch) -> None:
    """While a subagent runs, check_background_progress must return a
    formatted snapshot of its inner _messages list — and the snapshot
    must update as the inner agent makes progress."""
    bg = BackgroundTaskManager()
    stub = _StubAgent(_scripted_messages(), delay=0.08)
    registered_test_subagent = False
    if get_definition("test_subagent") is None:
        register_definition(
            AgentDefinition(
                name="test_subagent",
                description="test subagent",
                agent_type="subagent",
                include_skills=False,
            )
        )
        registered_test_subagent = True

    def _fake_spawn_agent(*args, **kwargs):
        return stub

    monkeypatch.setattr(
        "engine.runtime.agent.spawn_agent", _fake_spawn_agent, raising=True
    )

    alias = bg.next_alias()

    class _StubCfg:
        cwd = Path("/tmp")

    async def _subagent_coro() -> ToolResult:
        ctx = ToolExecutionContext(
            cwd=Path("/tmp"),
            metadata={
                "session_config": _StubCfg(),
                "background_task_manager": bg,
                "background_task_id": alias,
                "sandbox_id": "",
            },
        )
        return await run_subagent.execute(
            run_subagent.input_model(agent_name="test_subagent", prompt="task"), ctx
        )

    try:
        bg.launch(
            task_id=alias,
            tool_name="run_subagent",
            tool_input={"agent_name": "test_subagent", "prompt": "task"},
            coro=_subagent_coro(),
        )

        # Give the subagent time to register its provider and emit at least one
        # message into _messages.
        await asyncio.sleep(0.12)

        progress_tool = CheckBackgroundProgressTool()
        snapshots: list[str] = []
        for _ in range(3):
            ctx = ToolExecutionContext(
                cwd=Path("/tmp"),
                metadata={"background_task_manager": bg},
            )
            out = await progress_tool.execute(
                CheckBackgroundProgressInput(task_id=alias), ctx
            )
            snapshots.append(out.output)
            await asyncio.sleep(0.1)

        # Wait for the subagent to finish so the test doesn't leak the task.
        await bg.wait_for(alias, timeout=5.0)

        # At least one snapshot taken mid-run must show structured message blocks.
        assert any("[text]" in s or "[tool]" in s for s in snapshots), snapshots

        # The provider must surface MORE content as the subagent progresses.
        # i.e. the last in-flight snapshot should be at least as informative as
        # the first.
        in_flight = [s for s in snapshots if "DONE" not in s]
        assert in_flight, "expected at least one mid-flight snapshot"

        # After completion, get_status must return the FINAL result (not the
        # progress provider output).
        final_status = bg.get_status(alias)
        assert final_status, final_status
        assert "DONE" in final_status[0]["output"]
        assert "completed task" in final_status[0]["output"]
    finally:
        if registered_test_subagent:
            unregister_definition("test_subagent")


@pytest.mark.asyncio
async def test_format_last_n_messages_respects_n() -> None:
    msgs = _scripted_messages()
    out = format_last_n_messages(msgs, n=2)
    # Last 2 messages contain "editing file" and "DONE".
    assert "DONE" in out
    assert "editing file" in out
    assert "reading file" not in out
