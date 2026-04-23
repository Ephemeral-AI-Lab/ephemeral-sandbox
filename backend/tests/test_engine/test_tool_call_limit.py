"""Tests for ``tool_call_limit`` enforcement and the budget-warning reminder
(Phase 1 Step 3).

The engine loop is integration-heavy, so these tests target the small,
pure helpers around query budgeting and tool execution:

- :func:`_budget_warning_text` — fires only at the threshold and only
  while budget remains.
- ``execute_tool_call`` — counts every dispatch attempt and rejects
  with a structured error once the cap is reached.
"""

from __future__ import annotations

from pathlib import Path

import pytest

from agents.types import AgentDefinition
from engine.core.notifications import build_budget_warning
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


# ---------- build_budget_warning ---------------------------------------------


def test_budget_warning_none_when_no_limit():
    assert build_budget_warning(_ctx(None, 0)) is None


def test_budget_warning_silent_when_far_from_limit():
    # 100-call limit, 50 used → 50 remaining, threshold = 75 used. No warning.
    assert build_budget_warning(_ctx(100, 50)) is None


def test_budget_warning_fires_at_seventy_five_percent_used():
    # 100-call limit, 75 used → 75% of the budget consumed. Warns once.
    pair = build_budget_warning(_ctx(100, 75))
    assert pair is not None
    history_msg, event = pair
    assert event.category == "budget_warning"
    assert "25 of 100" in event.text
    assert "75 already used" in event.text


def test_budget_warning_fires_at_one_call_remaining():
    # 5-call limit, 4 used → 1 remaining. Warns regardless of percentage.
    pair = build_budget_warning(_ctx(5, 4))
    assert pair is not None
    _, event = pair
    assert "1 of 5" in event.text


def test_budget_warning_guides_planner_to_finalize_plan_handoff():
    ctx = _ctx(100, 75)
    ctx.tool_metadata["role"] = "planner"
    _, event = build_budget_warning(ctx)
    assert "submit_plan()" in event.text
    assert "strongest plan you can defend" in event.text


def test_budget_warning_guides_validator_to_wrap_up():
    ctx = _ctx(100, 75)
    ctx.tool_metadata["role"] = "reviewer"
    _, event = build_budget_warning(ctx)
    assert "submit_task_success()" in event.text
    assert "request_replan()" in event.text
    assert "diagnostics status" in event.text
    assert "Residual Risk line" not in event.text


def test_budget_warning_default_success_summary_requires_evidence():
    ctx = _ctx(100, 75)
    _, event = build_budget_warning(ctx)
    assert "Prepare to enter the terminal summarization flow soon" in event.text
    assert "diagnostics-only" in event.text
    assert "Use only evidence already gathered before this warning" in event.text
    assert "do not run one more verification" in event.text
    assert "verification was not already green" in event.text
    assert "collection, import, pytest-config, or environment failures" in event.text
    assert "A known next fix is not an exception" in event.text
    assert "non-terminal mutation or investigation" in event.text
    assert "latest required verification was already green after the final edit" in event.text
    assert "behavior/API delta" in event.text
    assert "exact commands and exit codes" in event.text
    assert "Residual Risk line" not in event.text


def test_budget_warning_emits_once_per_remaining_count():
    ctx = _ctx(10, 8)
    assert build_budget_warning(ctx) is not None
    assert build_budget_warning(ctx) is None


def test_budget_warning_silent_when_exhausted():
    # Exhausted: termination handles it, not the warning.
    assert build_budget_warning(_ctx(5, 5)) is None


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
    ctx = _ctx(limit=2, used=2, terminal_tools={"submit_plan"})
    ctx.tool_registry.get = lambda _name: None  # type: ignore[method-assign]
    result = await execute_tool_call(ctx, "submit_plan", "id1", {})

    assert result.is_error
    assert "Unknown tool" in result.content
    assert "tool_call_limit exceeded" not in result.content
    assert ctx.tool_calls_used == 2


@pytest.mark.asyncio
async def test_execute_tool_call_reserves_last_call_for_terminal_tool():
    ctx = _ctx(limit=2, used=1, terminal_tools={"submit_task_success"})
    ctx.tool_registry.get = lambda _name: None  # type: ignore[method-assign]

    result = await execute_tool_call(ctx, "daytona_read_file", "id1", {})

    assert result.is_error
    assert "terminal call reserved" in result.content
    assert "submit_task_success" in result.content
    assert ctx.tool_calls_used == 1


@pytest.mark.asyncio
async def test_execute_tool_call_unlimited_budget_does_not_count():
    ctx = _ctx(limit=None, used=0)
    ctx.tool_registry.get = lambda _name: None  # type: ignore[method-assign]
    await execute_tool_call(ctx, "ghost", "id1", {})
    # ``None`` limit short-circuits the budget gate; counter stays put.
    assert ctx.tool_calls_used == 0
