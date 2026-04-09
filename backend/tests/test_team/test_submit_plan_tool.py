"""Unit tests for tools.posthook.submit_plan.SubmitPlanTool."""

from __future__ import annotations

from pathlib import Path

import pytest

from team.models import Plan
from tools.core.base import ExecutionMetadata, ToolExecutionContext
from tools.posthook import SubmitPlanInput, SubmitPlanTool


@pytest.fixture(autouse=True)
def _all_agents_exist(monkeypatch):
    from team.planning import validation

    monkeypatch.setattr(validation, "_agent_exists", lambda name: True)


def _ctx() -> ToolExecutionContext:
    return ToolExecutionContext(cwd=Path.cwd(), metadata=ExecutionMetadata())


@pytest.mark.asyncio
async def test_valid_plan_accepted_and_stashed():
    tool = SubmitPlanTool()
    ctx = _ctx()
    args = SubmitPlanInput.model_validate(
        {
            "items": [
                {"agent_name": "developer", "local_id": "w1"},
                {"agent_name": "validator", "local_id": "w2", "deps": ["w1"]},
            ]
        }
    )
    res = await tool.execute(args, ctx)
    assert not res.is_error
    stashed = ctx.metadata["submitted_plan"]
    assert isinstance(stashed, Plan)
    assert len(stashed.items) == 2


@pytest.mark.asyncio
async def test_invalid_plan_returns_structured_error(monkeypatch):
    from team.planning import validation

    monkeypatch.setattr(validation, "_agent_exists", lambda name: name != "ghost")
    tool = SubmitPlanTool()
    ctx = _ctx()
    args = SubmitPlanInput.model_validate({"items": [{"agent_name": "ghost"}]})
    res = await tool.execute(args, ctx)
    assert res.is_error
    assert "unknown agent" in res.output
    assert "submitted_plan" not in ctx.metadata


@pytest.mark.asyncio
async def test_internal_cycle_rejected():
    tool = SubmitPlanTool()
    ctx = _ctx()
    args = SubmitPlanInput.model_validate(
        {
            "items": [
                {"agent_name": "a", "local_id": "w1", "deps": ["w2"]},
                {"agent_name": "a", "local_id": "w2", "deps": ["w1"]},
            ]
        }
    )
    res = await tool.execute(args, ctx)
    assert res.is_error
    assert "cycle" in res.output


@pytest.mark.asyncio
async def test_single_submission_guard():
    tool = SubmitPlanTool()
    ctx = _ctx()
    args = SubmitPlanInput.model_validate({"items": [{"agent_name": "developer"}]})
    res1 = await tool.execute(args, ctx)
    assert not res1.is_error
    res2 = await tool.execute(args, ctx)
    assert res2.is_error
    assert "already called" in res2.output


@pytest.mark.asyncio
async def test_max_plan_size_respects_metadata_override():
    tool = SubmitPlanTool()
    ctx = _ctx()
    ctx.metadata["max_plan_size"] = 1
    args = SubmitPlanInput.model_validate(
        {
            "items": [
                {"agent_name": "a", "local_id": "w1"},
                {"agent_name": "a", "local_id": "w2"},
            ]
        }
    )
    res = await tool.execute(args, ctx)
    assert res.is_error
    assert "max_plan_size" in res.output
