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


@pytest.mark.asyncio
async def test_canonical_scope_auto_injected_from_target_paths():
    tool = SubmitSummaryTool()
    ctx = _ctx()
    args = SubmitSummaryInput.model_validate(
        {
            "summary": "scout report",
            "artifact": {"target_paths": ["src/foo/", "./src/bar"], "files": []},
        }
    )
    res = await tool.execute(args, ctx)
    assert not res.is_error
    stashed = ctx.metadata["submitted_summary"]
    assert stashed.artifact["canonical_scope"] == "src/bar|src/foo"


@pytest.mark.asyncio
async def test_canonical_scope_explicit_value_preserved():
    tool = SubmitSummaryTool()
    ctx = _ctx()
    args = SubmitSummaryInput.model_validate(
        {
            "summary": "scout report",
            "artifact": {
                "target_paths": ["src/foo"],
                "canonical_scope": "explicit/key",
            },
        }
    )
    res = await tool.execute(args, ctx)
    assert not res.is_error
    assert ctx.metadata["submitted_summary"].artifact["canonical_scope"] == "explicit/key"


@pytest.mark.asyncio
async def test_canonical_scope_skipped_without_target_paths():
    tool = SubmitSummaryTool()
    ctx = _ctx()
    args = SubmitSummaryInput.model_validate(
        {"summary": "report", "artifact": {"files": []}}
    )
    res = await tool.execute(args, ctx)
    assert not res.is_error
    assert "canonical_scope" not in ctx.metadata["submitted_summary"].artifact


@pytest.mark.asyncio
async def test_snapshot_time_injected_from_work_item_start():
    tool = SubmitSummaryTool()
    ctx = _ctx()
    ctx.metadata["work_item_started_at"] = 1234.5
    args = SubmitSummaryInput.model_validate(
        {
            "summary": "scout report",
            "artifact": {"target_paths": ["src/foo"], "files": []},
        }
    )
    res = await tool.execute(args, ctx)
    assert not res.is_error
    assert ctx.metadata["submitted_summary"].artifact["snapshot_time"] == 1234.5


@pytest.mark.asyncio
async def test_snapshot_time_explicit_value_preserved():
    tool = SubmitSummaryTool()
    ctx = _ctx()
    ctx.metadata["work_item_started_at"] = 1234.5
    args = SubmitSummaryInput.model_validate(
        {
            "summary": "scout report",
            "artifact": {
                "target_paths": ["src/foo"],
                "snapshot_time": 99.0,
            },
        }
    )
    res = await tool.execute(args, ctx)
    assert not res.is_error
    assert ctx.metadata["submitted_summary"].artifact["snapshot_time"] == 99.0


@pytest.mark.asyncio
async def test_scout_artifact_missing_empty_contract_fields_gets_normalized():
    tool = SubmitSummaryTool()
    ctx = _ctx()
    args = SubmitSummaryInput.model_validate(
        {
            "summary": "scout report",
            "artifact": {
                "target_paths": ["src/auth"],
                "entry_points": ["src.auth:main"],
                "scope_coverage": 1.0,
            },
        }
    )
    res = await tool.execute(args, ctx)
    assert not res.is_error
    artifact = ctx.metadata["submitted_summary"].artifact
    assert artifact["files"] == []
    assert artifact["open_questions"] == []
    assert artifact["gaps"] == ""
    assert artifact["suggested_subdivisions"] == []


@pytest.mark.asyncio
async def test_scout_artifact_missing_scope_coverage_defaults_from_subdivisions():
    tool = SubmitSummaryTool()
    ctx = _ctx()
    args = SubmitSummaryInput.model_validate(
        {
            "summary": "scout report",
            "artifact": {
                "target_paths": ["src/pkg"],
                "suggested_subdivisions": ["src/pkg/io", "src/pkg/core"],
            },
        }
    )
    res = await tool.execute(args, ctx)
    assert not res.is_error
    artifact = ctx.metadata["submitted_summary"].artifact
    assert artifact["scope_coverage"] == 0.5
    assert artifact["files"] == []
    assert artifact["entry_points"] == []
    assert artifact["open_questions"] == []
    assert artifact["gaps"] == ""


@pytest.mark.asyncio
async def test_non_scout_artifact_is_not_normalized():
    tool = SubmitSummaryTool()
    ctx = _ctx()
    args = SubmitSummaryInput.model_validate(
        {
            "summary": "report",
            "artifact": {"files": ["a.py"]},
        }
    )
    res = await tool.execute(args, ctx)
    assert not res.is_error
    assert ctx.metadata["submitted_summary"].artifact == {"files": ["a.py"]}
