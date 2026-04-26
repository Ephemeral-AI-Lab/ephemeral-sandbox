"""Tests for ``tool_call_limit`` enforcement.

The engine loop is integration-heavy, so these tests target the small,
pure helpers around query budgeting and tool execution. ``execute_tool_call``
counts every dispatch attempt and rejects with a structured error once the
cap is reached.
"""

from __future__ import annotations

from pathlib import Path

import pytest

from agents.types import AgentDefinition
from engine.core.query import QueryContext
from tools.core.tool_execution import execute_tool_call
from tools.core.runtime import ExecutionMetadata


def _ctx(
    limit: int | None,
    used: int = 0,
    terminal_tools: set[str] | None = None,
) -> QueryContext:
    """Build a minimal QueryContext that only the budget paths inspect."""
    from unittest.mock import MagicMock

    return QueryContext(
        api_client=MagicMock(),
        tool_registry=MagicMock(),
        cwd=Path("."),
        model="m",
        system_prompt="p",
        max_tokens=1,
        tool_call_limit=limit,
        tool_calls_used=used,
        terminal_tools=terminal_tools or set(),
        tool_metadata=ExecutionMetadata(),
    )


# ---------- AgentDefinition --------------------------------------------------


def test_agent_definition_accepts_tool_call_limit():
    a = AgentDefinition(name="x", description="y", tool_call_limit=40)
    assert a.tool_call_limit == 40


def test_agent_definition_default_unlimited():
    assert AgentDefinition(name="x", description="y").tool_call_limit is None


def test_agent_definition_coerces_string():
    a = AgentDefinition.model_validate(
        {"name": "x", "description": "y", "tool_call_limit": "12"}
    )
    assert a.tool_call_limit == 12


def test_agent_definition_rejects_zero_and_negative():
    a = AgentDefinition(name="x", description="y", tool_call_limit=0)
    assert a.tool_call_limit is None
    a = AgentDefinition(name="x", description="y", tool_call_limit=-3)
    assert a.tool_call_limit is None


def test_agent_definition_rejects_removed_legacy_fields():
    with pytest.raises(ValueError):
        AgentDefinition.model_validate(
            {"name": "x", "description": "y", "effort": "high"}
        )


# ---------- execute_tool_call budget enforcement -----------------------------


@pytest.mark.asyncio
async def test_execute_tool_call_rejects_when_over_budget():
    ctx = _ctx(limit=2, used=2)
    result = await execute_tool_call(ctx, "any_tool", "id1", {})
    assert result.is_error
    assert "tool_call_limit exceeded" in result.content
    # Counter is NOT advanced past the cap on rejection.
    assert ctx.tool_calls_used == 2


@pytest.mark.asyncio
async def test_execute_tool_call_increments_counter_on_unknown_tool():
    """Counting happens at dispatch attempt, before tool resolution."""
    ctx = _ctx(limit=10, used=0)
    # The mock tool registry returns None → "Unknown tool" path. The
    # counter should still have incremented because dispatch was attempted.
    ctx.tool_registry.get = lambda _name: None  # type: ignore[method-assign]
    result = await execute_tool_call(ctx, "ghost", "id1", {})
    assert result.is_error
    assert "Unknown tool" in result.content
    assert ctx.tool_calls_used == 1


@pytest.mark.asyncio
async def test_execute_tool_call_allows_terminal_tool_when_budget_exhausted():
    ctx = _ctx(limit=2, used=2, terminal_tools={"submit_plan_handoff"})
    ctx.tool_registry.get = lambda _name: None  # type: ignore[method-assign]
    result = await execute_tool_call(ctx, "submit_plan_handoff", "id1", {})

    assert result.is_error
    assert "Unknown tool" in result.content
    assert "tool_call_limit exceeded" not in result.content
    assert ctx.tool_calls_used == 2


@pytest.mark.asyncio
async def test_execute_tool_call_reserves_last_call_for_terminal_tool():
    ctx = _ctx(limit=2, used=1, terminal_tools={"submit_task_completion"})
    ctx.tool_registry.get = lambda _name: None  # type: ignore[method-assign]

    result = await execute_tool_call(ctx, "read_file", "id1", {})

    assert result.is_error
    assert "terminal call reserved" in result.content
    assert "submit_task_completion" in result.content
    assert ctx.tool_calls_used == 1


@pytest.mark.asyncio
async def test_execute_tool_call_unlimited_budget_does_not_count():
    ctx = _ctx(limit=None, used=0)
    ctx.tool_registry.get = lambda _name: None  # type: ignore[method-assign]
    await execute_tool_call(ctx, "ghost", "id1", {})
    # ``None`` limit short-circuits the budget gate; counter stays put.
    assert ctx.tool_calls_used == 0


# ---------- 75% budget warning notification ---------------------------------


def _ctx_with_notifications(limit: int, used: int = 0) -> tuple[QueryContext, list]:
    """Build a context wired to a SystemNotificationService and capture emits."""
    from notification.service import SystemNotificationService

    captured: list = []

    async def _emit(event):
        captured.append(event)

    service = SystemNotificationService(emit=_emit)
    ctx = _ctx(limit=limit, used=used)
    assert ctx.tool_metadata is not None
    ctx.tool_metadata.system_notification_service = service
    ctx.tool_registry.get = lambda _name: None  # type: ignore[method-assign]
    return ctx, captured


@pytest.mark.asyncio
async def test_budget_warning_fires_once_when_crossing_75_percent():
    ctx, captured = _ctx_with_notifications(limit=4, used=2)
    # 2 -> 3 puts us at 75% exactly; warning fires.
    await execute_tool_call(ctx, "any_tool", "id1", {})
    assert ctx.tool_calls_used == 3
    assert ctx.tool_budget_warning_fired is True
    assert len(captured) == 1
    assert "75%" in captured[0].text
    assert "3/4" in captured[0].text
    assert captured[0].category == "tool_budget"


@pytest.mark.asyncio
async def test_budget_warning_does_not_refire_on_subsequent_calls():
    ctx, captured = _ctx_with_notifications(limit=4, used=2)
    await execute_tool_call(ctx, "a", "id1", {})  # crosses 75%
    await execute_tool_call(ctx, "b", "id2", {})  # 4/4 — at cap, but already fired
    assert len(captured) == 1


@pytest.mark.asyncio
async def test_budget_warning_silent_below_threshold():
    ctx, captured = _ctx_with_notifications(limit=10, used=0)
    # 0 -> 1 -> 2 -> ... only fires once we hit ceil(10*0.75)=8.
    for i in range(7):
        await execute_tool_call(ctx, "t", f"id{i}", {})
    assert ctx.tool_calls_used == 7
    assert captured == []
    await execute_tool_call(ctx, "t", "id8", {})  # 8/10 = 0.8
    assert len(captured) == 1


@pytest.mark.asyncio
async def test_budget_warning_skipped_when_no_service():
    # Default _ctx has no notification service attached.
    ctx = _ctx(limit=4, used=2)
    ctx.tool_registry.get = lambda _name: None  # type: ignore[method-assign]
    await execute_tool_call(ctx, "any_tool", "id1", {})
    # No crash, flag stays False since nothing was fired.
    assert ctx.tool_budget_warning_fired is False


@pytest.mark.asyncio
async def test_budget_warning_skipped_when_no_limit():
    ctx, captured = _ctx_with_notifications(limit=4, used=0)
    ctx.tool_call_limit = None
    await execute_tool_call(ctx, "any_tool", "id1", {})
    assert captured == []
    assert ctx.tool_budget_warning_fired is False
