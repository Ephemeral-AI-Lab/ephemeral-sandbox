"""Tests for engine.core.tool_batch.validate_tool_batch."""

from __future__ import annotations

from types import SimpleNamespace

from engine.core.tool_batch import validate_tool_batch
from message.messages import ToolUseBlock


def _ctx(
    terminal_tools: set[str] | None = None,
    tool_metadata=None,
) -> SimpleNamespace:
    return SimpleNamespace(
        terminal_tools=terminal_tools or set(),
        tool_metadata=tool_metadata,
    )


def _tool(name: str, **input_kwargs) -> ToolUseBlock:
    return ToolUseBlock(name=name, input=input_kwargs)


def test_validate_tool_batch_allows_terminal_tool_alone():
    ctx = _ctx(terminal_tools={"submit_task_completion"})
    result = validate_tool_batch(ctx, [_tool("submit_task_completion")])
    assert result is None


def test_validate_tool_batch_allows_non_terminal_batch():
    ctx = _ctx(terminal_tools={"submit_task_completion"})
    result = validate_tool_batch(ctx, [_tool("read_file"), _tool("grep")])
    assert result is None


def test_validate_tool_batch_rejects_terminal_with_sibling():
    ctx = _ctx(terminal_tools={"submit_task_completion"})
    calls = [_tool("submit_task_completion"), _tool("read_file")]
    result = validate_tool_batch(ctx, calls)
    assert result is not None
    assert len(result) == len(calls)
    for block, call in zip(result, calls, strict=True):
        assert block.is_error is True
        assert block.tool_use_id == call.id
        assert "Terminal tool" in block.content
        assert "submit_task_completion" in block.content


def test_validate_tool_batch_rejects_even_when_terminal_last():
    ctx = _ctx(terminal_tools={"submit_task_completion"})
    calls = [_tool("read_file"), _tool("submit_task_completion")]
    result = validate_tool_batch(ctx, calls)
    assert result is not None
    assert all(block.is_error for block in result)


def test_validate_tool_batch_no_terminal_tools_configured():
    """When terminal_tools is empty, siblings pass through freely."""
    ctx = _ctx(terminal_tools=set())
    result = validate_tool_batch(
        ctx, [_tool("submit_task_completion"), _tool("read_file")]
    )
    assert result is None


def test_validate_tool_batch_empty_calls():
    ctx = _ctx(terminal_tools={"submit_task_completion"})
    assert validate_tool_batch(ctx, []) is None
