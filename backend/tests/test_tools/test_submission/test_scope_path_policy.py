"""Tests for submission pre-hooks."""

from __future__ import annotations

from pathlib import Path

import pytest

from message.stream_events import StreamEvent
from tools.core.base import ToolExecutionContext
from tools.core.hooks import ToolHookRegistry, run_pre_hooks
from tools.submission.hooks.prehook import scope_path_policy
from tools.submission.tools import SubmitPlanTool, SubmitReplanTool

pytestmark = pytest.mark.asyncio


_TASK_SPEC = {
    "goal": "Implement the production change.",
    "detail": "Edit only the production owner file.",
    "acceptance_criteria": "Run the named verification command.",
}


async def _capture_emit(events: list[StreamEvent], event: StreamEvent) -> None:
    events.append(event)


def _context() -> ToolExecutionContext:
    return ToolExecutionContext(cwd=Path("/tmp"))


async def _run_submission_pre_hook(tool_name: str, args) -> object:
    registry = ToolHookRegistry()
    scope_path_policy.register(registry)
    events: list[StreamEvent] = []
    result = await run_pre_hooks(
        tool_name,
        args,
        _context(),
        emit=lambda event: _capture_emit(events, event),
        registry=registry,
    )
    assert events == []
    return result


async def test_submit_plan_prehook_rejects_test_file_scope_paths() -> None:
    args = SubmitPlanTool.input_model(
        new_tasks=[
            {
                "id": "impl",
                "name": "developer",
                "spec": _TASK_SPEC,
                "scope_paths": ["src/api.py", "pkg/tests/test_owner.py"],
            }
        ],
    )

    result = await _run_submission_pre_hook("submit_plan", args)

    assert result.has_error is True
    assert "test files and test directories cannot be used as scope_paths" in (
        result.error_message or ""
    )
    assert "impl: pkg/tests/test_owner.py" in (result.error_message or "")


async def test_submit_replan_prehook_rejects_test_directory_scope_paths() -> None:
    args = SubmitReplanTool.input_model(
        new_tasks=[
            {
                "id": "repair",
                "name": "developer",
                "spec": _TASK_SPEC,
                "scope_paths": ["backend/tests"],
            }
        ],
        cancel_ids=[],
    )

    result = await _run_submission_pre_hook("submit_replan", args)

    assert result.has_error is True
    assert "repair: backend/tests" in (result.error_message or "")


async def test_submission_prehook_allows_production_scope_paths() -> None:
    args = SubmitPlanTool.input_model(
        new_tasks=[
            {
                "id": "impl",
                "name": "developer",
                "spec": _TASK_SPEC,
                "scope_paths": ["src/api.py", "src/service/auth.ts"],
            }
        ],
    )

    result = await _run_submission_pre_hook("submit_plan", args)

    assert result.has_error is False
    assert result.tool_input is args
