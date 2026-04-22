"""Unit tests for the platform hook pipeline."""

from __future__ import annotations

from pathlib import Path

import pytest
from pydantic import BaseModel

from message.stream_events import StreamEvent, SystemNotification
from tools.core.base import ToolExecutionContext, ToolResult
from tools.core.hooks import (
    PostHookOutcome,
    PreHookOutcome,
    ToolHookRegistry,
    run_post_hooks,
    run_pre_hooks,
)

pytestmark = pytest.mark.asyncio


class _Args(BaseModel):
    value: str = ""


def _context() -> ToolExecutionContext:
    return ToolExecutionContext(cwd=Path("/tmp"))


async def _capture_emit(events: list[StreamEvent], event: StreamEvent) -> None:
    events.append(event)


async def test_pre_empty_registry_is_noop() -> None:
    reg = ToolHookRegistry()
    args = _Args(value="original")
    events: list[StreamEvent] = []

    result = await run_pre_hooks(
        "tool",
        args,
        _context(),
        emit=lambda event: _capture_emit(events, event),
        registry=reg,
    )

    assert result.has_error is False
    assert result.tool_input is args
    assert events == []


async def test_pre_mutation_threads_to_later_hooks() -> None:
    reg = ToolHookRegistry()
    seen: list[str] = []
    events: list[StreamEvent] = []

    async def first(tool_name, args, context):
        return PreHookOutcome(tool_input=_Args(value="mutated"))

    async def second(tool_name, args, context):
        seen.append(args.value)
        return PreHookOutcome()

    reg.register("*", "pre", 10, first, name="first")
    reg.register("*", "pre", 20, second, name="second")

    result = await run_pre_hooks(
        "tool",
        _Args(value="original"),
        _context(),
        emit=lambda event: _capture_emit(events, event),
        registry=reg,
    )

    assert result.has_error is False
    assert result.tool_input.value == "mutated"
    assert seen == ["mutated"]
    assert events == []


async def test_pre_advisories_emit_immediately_and_separately() -> None:
    reg = ToolHookRegistry()
    events: list[StreamEvent] = []

    async def first(tool_name, args, context):
        return PreHookOutcome(advisories=("one", "two"))

    async def second(tool_name, args, context):
        return PreHookOutcome(advisories=("three",))

    reg.register("*", "pre", 10, first, name="first")
    reg.register("*", "pre", 20, second, name="second")

    result = await run_pre_hooks(
        "daytona_codeact",
        _Args(),
        _context(),
        emit=lambda event: _capture_emit(events, event),
        registry=reg,
    )

    assert result.has_error is False
    assert [type(event) for event in events] == [
        SystemNotification,
        SystemNotification,
        SystemNotification,
    ]
    assert [event.text for event in events if isinstance(event, SystemNotification)] == [
        "[pre-hook tip] daytona_codeact: one",
        "[pre-hook tip] daytona_codeact: two",
        "[pre-hook tip] daytona_codeact: three",
    ]


async def test_pre_denial_short_circuits_without_advisory_batch() -> None:
    reg = ToolHookRegistry()
    called_after = False
    events: list[StreamEvent] = []

    async def adv(tool_name, args, context):
        return PreHookOutcome(advisories=("before",))

    async def blocker(tool_name, args, context):
        return PreHookOutcome(has_error=True, error_message="blocked")

    async def later(tool_name, args, context):
        nonlocal called_after
        called_after = True
        return PreHookOutcome()

    reg.register("*", "pre", 10, adv, name="adv")
    reg.register("*", "pre", 20, blocker, name="blocker")
    reg.register("*", "pre", 30, later, name="later")

    result = await run_pre_hooks(
        "tool",
        _Args(),
        _context(),
        emit=lambda event: _capture_emit(events, event),
        registry=reg,
    )

    assert result.has_error is True
    assert result.error_message == "blocked"
    assert called_after is False
    assert [event.text for event in events if isinstance(event, SystemNotification)] == [
        "[pre-hook tip] tool: before"
    ]


async def test_pre_hook_exception_becomes_pipeline_error() -> None:
    reg = ToolHookRegistry()
    events: list[StreamEvent] = []

    async def broken(tool_name, args, context):
        raise RuntimeError("boom")

    reg.register("*", "pre", 10, broken, name="broken")

    result = await run_pre_hooks(
        "tool",
        _Args(),
        _context(),
        emit=lambda event: _capture_emit(events, event),
        registry=reg,
    )

    assert result.has_error is True
    assert result.error_message == "broken: boom"
    assert events == []


async def test_post_advisory_and_denial_ordering() -> None:
    reg = ToolHookRegistry()
    events: list[StreamEvent] = []
    called_after = False

    async def adv(tool_name, args, context, result):
        return PostHookOutcome(advisories=("post-warn",))

    async def blocker(tool_name, args, context, result):
        return PostHookOutcome(has_error=True, error_message="post blocked")

    async def later(tool_name, args, context, result):
        nonlocal called_after
        called_after = True
        return PostHookOutcome()

    reg.register("*", "post", 10, adv, name="adv")
    reg.register("*", "post", 20, blocker, name="blocker")
    reg.register("*", "post", 30, later, name="later")

    outcome = await run_post_hooks(
        "tool",
        _Args(),
        _context(),
        ToolResult(output="ok"),
        emit=lambda event: _capture_emit(events, event),
        registry=reg,
    )

    assert outcome.has_error is True
    assert outcome.error_message == "post blocked"
    assert called_after is False
    assert [event.text for event in events if isinstance(event, SystemNotification)] == [
        "[post-hook advisory] tool: post-warn"
    ]


async def test_registry_registration_is_idempotent_by_key() -> None:
    reg = ToolHookRegistry()

    async def hook(tool_name, args, context):
        return PreHookOutcome()

    reg.register("tool", "pre", 10, hook, name="same")
    reg.register("tool", "pre", 10, hook, name="same")

    assert len(reg.matching("tool", "pre")) == 1
