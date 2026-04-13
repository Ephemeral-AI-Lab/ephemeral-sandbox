"""Tests for posthook coordination warning gating."""

from __future__ import annotations

from pathlib import Path

import pytest

from team.models import BlockerDeclaration, ReplanPlan
import tools.context.freshness as freshness_module
from tools.core.base import ToolExecutionContext
from tools.context.freshness import FreshnessReport
from tools.posthook.toolkit import (
    AddTasksTool,
    CancelAndRedraftTool,
    DeclareBlockerTool,
    RequestRetryTool,
    SubmitSummaryTool,
)


def _ctx(metadata=None) -> ToolExecutionContext:
    return ToolExecutionContext(cwd=Path("/tmp"), metadata=metadata or {})


@pytest.mark.asyncio
async def test_submit_summary_allows_tainted_coordination_packet():
    """Coordination warnings are advisory — they must not block submission."""
    ctx = _ctx(
        {
            "coordination_warnings": [
                {
                    "category": "write_scope",
                    "message": (
                        "daytona_write_file: write to dask/_compatibility.py is outside "
                        "write_scope ['dask/compatibility.py'] (advisory)."
                    ),
                }
            ]
        }
    )

    result = await SubmitSummaryTool().execute(
        SubmitSummaryTool.input_model(summary="patched compatibility handling"),
        ctx,
    )

    assert not result.is_error
    assert "accepted" in result.output


@pytest.mark.asyncio
async def test_request_retry_allows_tainted_coordination_packet():
    """Coordination warnings are advisory — they must not block retry."""
    ctx = _ctx(
        {
            "coordination_warnings": [
                {
                    "category": "write_scope",
                    "message": (
                        "daytona_codeact.write: write to dask/_compatibility.py is outside "
                        "write_scope ['dask/compatibility.py'] (advisory)."
                    ),
                }
            ]
        }
    )

    result = await RequestRetryTool().execute(
        RequestRetryTool.input_model(reason="rerun pytest"),
        ctx,
    )

    assert not result.is_error
    assert "Retry requested" in result.output


@pytest.mark.asyncio
async def test_submit_summary_rejects_stale_context(monkeypatch):
    async def _stale(_context):
        return FreshnessReport(new_dep_notes=1)

    monkeypatch.setattr(freshness_module, "check_freshness", _stale)
    ctx = _ctx({"checked_context_freshness": False})

    result = await SubmitSummaryTool().execute(
        SubmitSummaryTool.input_model(summary="done"),
        ctx,
    )

    assert result.is_error
    assert "context_changed_since()" in result.output
    assert "request_replan()" in result.output


@pytest.mark.asyncio
async def test_add_tasks_sets_replan_submission():
    ctx = _ctx({})

    result = await AddTasksTool().execute(
        AddTasksTool.input_model(
            add_tasks=[{"id": "fix-1", "task": "fix owner", "agent": "developer"}],
            cancel_ids=[],
        ),
        ctx,
    )

    assert not result.is_error
    submitted = ctx.metadata["submitted_output"]
    assert isinstance(submitted, ReplanPlan)
    assert [task.id for task in submitted.add_tasks] == ["fix-1"]
    assert submitted.cancel_ids == []


@pytest.mark.asyncio
async def test_declare_blocker_sets_blocker_submission():
    ctx = _ctx({})

    result = await DeclareBlockerTool().execute(
        DeclareBlockerTool.input_model(
            root_cause_paths=["pkg/shared.py"],
            reason="shared import crash",
            suggestion="restore exported helper",
        ),
        ctx,
    )

    assert not result.is_error
    submitted = ctx.metadata["submitted_output"]
    assert isinstance(submitted, BlockerDeclaration)
    assert submitted.root_cause_paths == ["pkg/shared.py"]
    assert submitted.reason == "shared import crash"


@pytest.mark.asyncio
async def test_cancel_and_redraft_sets_replan_submission():
    ctx = _ctx({})

    result = await CancelAndRedraftTool().execute(
        CancelAndRedraftTool.input_model(
            add_tasks=[{"id": "fix-2", "task": "rewrite lane", "agent": "developer"}],
            cancel_ids=["old-1"],
        ),
        ctx,
    )

    assert not result.is_error
    submitted = ctx.metadata["submitted_output"]
    assert isinstance(submitted, ReplanPlan)
    assert [task.id for task in submitted.add_tasks] == ["fix-2"]
    assert submitted.cancel_ids == ["old-1"]
