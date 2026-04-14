"""Tests for posthook submission tools."""

from __future__ import annotations

import asyncio
from pathlib import Path

import pytest

from team.models import BlockerDeclaration, ReplanPlan
from tools.context.toolkit import PostNoteTool
from tools.core.base import ToolExecutionContext
from tools.posthook.toolkit import (
    AddTasksTool,
    CancelAndRedraftTool,
    DeclareBlockerTool,
    PosthookTools,
    RequestReplanTool,
)


class _FakeTaskCenter:
    async def post(self, note):
        pass


def _ctx(metadata=None) -> ToolExecutionContext:
    return ToolExecutionContext(cwd=Path("/tmp"), metadata=metadata or {})


def test_post_note_accepts_content():
    """PostNoteTool accepts content and posts note."""
    ctx = _ctx({"task_center": _FakeTaskCenter()})

    result = asyncio.run(PostNoteTool().execute(
        PostNoteTool.input_model(content="patched compatibility handling"),
        ctx,
    ))

    assert not result.is_error
    assert "posted" in result.output.lower()


def test_post_note_rejects_empty_content():
    """PostNoteTool requires non-empty content."""
    with pytest.raises(Exception):
        PostNoteTool.input_model(content="")


def test_add_tasks_sets_replan_submission():
    ctx = _ctx({})

    result = asyncio.run(AddTasksTool().execute(
        AddTasksTool.input_model(
            add_tasks=[{"id": "fix-1", "task": "fix owner", "agent": "developer"}],
            cancel_ids=[],
        ),
        ctx,
    ))

    assert not result.is_error
    submitted = ctx.metadata["submitted_output"]
    assert isinstance(submitted, ReplanPlan)
    assert [task.id for task in submitted.add_tasks] == ["fix-1"]
    assert submitted.cancel_ids == []


def test_declare_blocker_sets_blocker_submission():
    ctx = _ctx({})

    result = asyncio.run(DeclareBlockerTool().execute(
        DeclareBlockerTool.input_model(
            root_cause_paths=["pkg/shared.py"],
            reason="shared import crash",
            suggestion="restore exported helper",
        ),
        ctx,
    ))

    assert not result.is_error
    submitted = ctx.metadata["submitted_output"]
    assert isinstance(submitted, BlockerDeclaration)
    assert submitted.root_cause_paths == ["pkg/shared.py"]
    assert submitted.reason == "shared import crash"


def test_cancel_and_redraft_sets_replan_submission():
    ctx = _ctx({})

    result = asyncio.run(CancelAndRedraftTool().execute(
        CancelAndRedraftTool.input_model(
            add_tasks=[{"id": "fix-2", "task": "rewrite lane", "agent": "developer"}],
            cancel_ids=["old-1"],
        ),
        ctx,
    ))

    assert not result.is_error
    submitted = ctx.metadata["submitted_output"]
    assert isinstance(submitted, ReplanPlan)
    assert [task.id for task in submitted.add_tasks] == ["fix-2"]
    assert submitted.cancel_ids == ["old-1"]


def test_posthook_tools_resolver_role_gets_terminal_submission_tools():
    ctx = _ctx({"role": "resolver"})

    toolkit = PosthookTools.from_context(ctx)

    assert [tool.name for tool in toolkit.list_tools()] == [
        PostNoteTool.name,
        RequestReplanTool.name,
    ]
