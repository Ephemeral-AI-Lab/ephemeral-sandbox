"""Tests for Task Center pre-hooks."""

from __future__ import annotations

from pathlib import Path

import pytest

from message.stream_events import StreamEvent
from tools.core.base import ToolExecutionContext
from tools.core.hooks import ToolHookRegistry, run_pre_hooks
from tools.task_center.hooks.prehook import scout_file_note_coverage_policy
from tools.task_center.toolkit import SubmitFileNotesTool

pytestmark = pytest.mark.asyncio


async def _capture_emit(events: list[StreamEvent], event: StreamEvent) -> None:
    events.append(event)


def _context(*, agent_name: str, write_scope: list[str] | None = None) -> ToolExecutionContext:
    metadata = {"agent_name": agent_name}
    if write_scope is not None:
        metadata["write_scope"] = write_scope
    return ToolExecutionContext(cwd=Path("/tmp"), metadata=metadata)


async def _run_file_note_pre_hook(context: ToolExecutionContext, args) -> object:
    registry = ToolHookRegistry()
    scout_file_note_coverage_policy.register(registry)
    events: list[StreamEvent] = []
    result = await run_pre_hooks(
        "submit_file_notes",
        args,
        context,
        emit=lambda event: _capture_emit(events, event),
        registry=registry,
    )
    assert events == []
    return result


async def test_scout_file_note_prehook_allows_exact_scope_coverage() -> None:
    args = SubmitFileNotesTool.input_model(
        notes=[
            {"path": "./pkg/auth.py/", "content": "Auth summary."},
            {"path": "pkg/session.py", "content": "Session summary."},
        ],
    )

    result = await _run_file_note_pre_hook(
        _context(agent_name="scout", write_scope=["pkg/auth.py", "pkg/session.py"]),
        args,
    )

    assert result.has_error is False
    assert result.tool_input is args


async def test_scout_file_note_prehook_rejects_missing_assigned_path() -> None:
    args = SubmitFileNotesTool.input_model(
        notes=[{"path": "pkg/auth.py", "content": "Auth summary."}],
    )

    result = await _run_file_note_pre_hook(
        _context(agent_name="scout", write_scope=["pkg/auth.py", "pkg/session.py"]),
        args,
    )

    assert result.has_error is True
    assert "Missing: pkg/session.py." in (result.error_message or "")


async def test_scout_file_note_prehook_rejects_extra_unassigned_path() -> None:
    args = SubmitFileNotesTool.input_model(
        notes=[
            {"path": "pkg/auth.py", "content": "Auth summary."},
            {"path": "pkg/session.py", "content": "Session summary."},
        ],
    )

    result = await _run_file_note_pre_hook(
        _context(agent_name="scout", write_scope=["pkg/auth.py"]),
        args,
    )

    assert result.has_error is True
    assert "Unexpected: pkg/session.py." in (result.error_message or "")


async def test_scout_file_note_prehook_rejects_descendant_substitution_for_directory() -> None:
    args = SubmitFileNotesTool.input_model(
        notes=[{"path": "pkg/auth.py", "content": "Auth summary."}],
    )

    result = await _run_file_note_pre_hook(
        _context(agent_name="scout", write_scope=["pkg"]),
        args,
    )

    assert result.has_error is True
    error = result.error_message or ""
    assert "Missing: pkg." in error
    assert "Unexpected: pkg/auth.py." in error


async def test_non_scout_file_note_prehook_is_noop() -> None:
    args = SubmitFileNotesTool.input_model(
        notes=[{"path": "pkg/auth.py", "content": "Auth summary."}],
    )

    result = await _run_file_note_pre_hook(
        _context(agent_name="developer", write_scope=["pkg/auth.py", "pkg/session.py"]),
        args,
    )

    assert result.has_error is False
    assert result.tool_input is args
