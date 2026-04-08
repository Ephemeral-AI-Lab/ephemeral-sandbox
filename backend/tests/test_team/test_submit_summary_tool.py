"""Unit tests for tools.posthook.submit_summary.SubmitSummaryTool."""

from __future__ import annotations

from pathlib import Path

import pytest
from pydantic import ValidationError

from tools.core.base import ExecutionMetadata, ToolExecutionContext
from tools.posthook import (
    SubmittedSummary,
    SubmitSummaryInput,
    SubmitSummaryTool,
)


def _ctx() -> ToolExecutionContext:
    return ToolExecutionContext(cwd=Path.cwd(), metadata=ExecutionMetadata())


@pytest.mark.asyncio
async def test_summary_accepted_and_stashed_default_key():
    tool = SubmitSummaryTool()
    ctx = _ctx()
    args = SubmitSummaryInput.model_validate(
        {"summary": "Refactored auth; 3 files changed."}
    )
    res = await tool.execute(args, ctx)
    assert not res.is_error
    stashed = ctx.metadata["submitted_summary"]
    assert isinstance(stashed, SubmittedSummary)
    assert stashed.summary == "Refactored auth; 3 files changed."
    assert stashed.artifact is None


@pytest.mark.asyncio
async def test_summary_with_artifact_preserved():
    tool = SubmitSummaryTool()
    ctx = _ctx()
    args = SubmitSummaryInput.model_validate(
        {
            "summary": "Investigated flaky test.",
            "artifact": {"files": ["a.py", "b.py"], "verdict": "race"},
        }
    )
    res = await tool.execute(args, ctx)
    assert not res.is_error
    stashed = ctx.metadata["submitted_summary"]
    assert isinstance(stashed, SubmittedSummary)
    assert stashed.artifact == {"files": ["a.py", "b.py"], "verdict": "race"}


@pytest.mark.asyncio
async def test_respects_posthook_metadata_key_override():
    tool = SubmitSummaryTool()
    ctx = _ctx()
    ctx.metadata["posthook_metadata_key"] = "custom_slot"
    args = SubmitSummaryInput.model_validate({"summary": "done."})
    res = await tool.execute(args, ctx)
    assert not res.is_error
    assert "custom_slot" in ctx.metadata
    assert "submitted_summary" not in ctx.metadata


@pytest.mark.asyncio
async def test_empty_summary_rejected_by_schema():
    # Pydantic min_length=1 rejects before execute() is ever called.
    with pytest.raises(ValidationError):
        SubmitSummaryInput.model_validate({"summary": ""})


@pytest.mark.asyncio
async def test_whitespace_only_summary_rejected_by_tool():
    tool = SubmitSummaryTool()
    ctx = _ctx()
    args = SubmitSummaryInput.model_validate({"summary": "   "})
    res = await tool.execute(args, ctx)
    assert res.is_error
    assert "non-empty" in res.output
    assert "submitted_summary" not in ctx.metadata


@pytest.mark.asyncio
async def test_single_submission_guard():
    tool = SubmitSummaryTool()
    ctx = _ctx()
    args = SubmitSummaryInput.model_validate({"summary": "first."})
    res1 = await tool.execute(args, ctx)
    assert not res1.is_error
    res2 = await tool.execute(args, ctx)
    assert res2.is_error
    assert "already called" in res2.output
    # Stashed payload unchanged.
    assert ctx.metadata["submitted_summary"].summary == "first."
