"""Tests for posthook coordination warning gating."""

from __future__ import annotations

from pathlib import Path

import pytest

from tools.core.base import ToolExecutionContext
from tools.posthook.toolkit import RequestRetryTool, SubmitSummaryTool


def _ctx(metadata=None) -> ToolExecutionContext:
    return ToolExecutionContext(cwd=Path("/tmp"), metadata=metadata or {})


@pytest.mark.asyncio
async def test_submit_summary_rejects_tainted_coordination_packet():
    ctx = _ctx(
        {
            "coordination_warnings": [
                {
                    "category": "write_scope",
                    "message": (
                        "daytona_write_file: write to dask/_compatibility.py is outside "
                        "write_scope ['dask/compatibility.py'] (advisory mode)."
                    ),
                }
            ]
        }
    )

    result = await SubmitSummaryTool().execute(
        SubmitSummaryTool.input_model(summary="patched compatibility handling"),
        ctx,
    )

    assert result.is_error
    assert "request_replan()" in result.output
    assert "tainted this task packet" in result.output


@pytest.mark.asyncio
async def test_request_retry_rejects_tainted_coordination_packet():
    ctx = _ctx(
        {
            "coordination_warnings": [
                {
                    "category": "write_scope",
                    "message": (
                        "daytona_codeact.write: write to dask/_compatibility.py is outside "
                        "write_scope ['dask/compatibility.py'] (advisory mode)."
                    ),
                }
            ]
        }
    )

    result = await RequestRetryTool().execute(
        RequestRetryTool.input_model(reason="rerun pytest"),
        ctx,
    )

    assert result.is_error
    assert "request_replan()" in result.output
    assert "outside write_scope" in result.output
