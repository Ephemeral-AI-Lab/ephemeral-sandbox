"""Tests for the tool-surface authorization gate."""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path

import pytest
from pydantic import BaseModel

from agents.types import ModeDefinition
from message.messages import ToolResultBlock
from tools.core.base import BaseTool, ToolRegistry, ToolResult
from tools.core.runtime import ExecutionMetadata
from tools.core.tool_execution import evaluate_mode_gate, execute_tool_call_streaming


def test_gate_allows_when_active_mode_is_none() -> None:
    assert evaluate_mode_gate(None, "anything", "id-1") is None


def test_gate_terminal_always_allowed() -> None:
    surface = ModeDefinition(
        name="direct",
        is_default=True,
        allowed_tools=[],
        terminals=["submit_plan_handoff"],
    )

    assert evaluate_mode_gate(surface, "submit_plan_handoff", "id-1") is None


def test_gate_allowed_tools_list_gates_unknown() -> None:
    surface = ModeDefinition(
        name="direct",
        is_default=True,
        allowed_tools=["read"],
        terminals=["submit_task_completion"],
    )

    assert evaluate_mode_gate(surface, "read", "id-1") is None
    deny = evaluate_mode_gate(surface, "write", "id-1")
    assert deny is not None and deny.is_error


def test_gate_deny_payload_format() -> None:
    surface = ModeDefinition(
        name="direct",
        is_default=True,
        allowed_tools=[],
        terminals=["submit_task_completion"],
    )
    deny = evaluate_mode_gate(surface, "submit_plan_handoff", "id-1")

    assert deny is not None
    assert "submit_plan_handoff" in deny.content
    assert "direct" in deny.content
    assert "submit_task_completion" in deny.content
    assert deny.is_error
    assert deny.tool_use_id == "id-1"


class _NoopInput(BaseModel):
    pass


class _AllowedTool(BaseTool):
    name = "allowed_tool"
    description = "ok"
    input_model = _NoopInput

    async def execute(self, args, ctx):  # type: ignore[override]
        return ToolResult(output="ran")


@dataclass
class _StubContext:
    """Just enough of QueryContext for execute_tool_call_streaming to run."""

    tool_registry: ToolRegistry
    cwd: Path
    tool_call_limit: int | None
    tool_calls_used: int = 0
    tool_budget_warning_fired: bool = False
    terminal_tools: set = None  # type: ignore[assignment]
    tool_metadata: ExecutionMetadata = None  # type: ignore[assignment]
    active_mode: ModeDefinition | None = None

    def __post_init__(self) -> None:
        if self.terminal_tools is None:
            self.terminal_tools = set()
        if self.tool_metadata is None:
            self.tool_metadata = ExecutionMetadata()


@pytest.mark.asyncio
async def test_mode_deny_does_not_consume_budget() -> None:
    surface = ModeDefinition(
        name="direct",
        is_default=True,
        allowed_tools=["read"],
        terminals=["submit_plan_handoff"],
    )
    registry = ToolRegistry()
    registry.register(_AllowedTool())
    ctx = _StubContext(
        tool_registry=registry,
        cwd=Path("/tmp"),
        tool_call_limit=5,
        active_mode=surface,
    )

    async def _emit(_event):
        pass

    res = await execute_tool_call_streaming(
        ctx,  # type: ignore[arg-type]
        "unauthorized_tool",
        "tu-1",
        {},
        emit=_emit,
        emit_started=False,
    )

    assert isinstance(res, ToolResultBlock)
    assert res.is_error
    assert "not allowed" in res.content
    assert ctx.tool_calls_used == 0


@pytest.mark.asyncio
async def test_allowed_tool_consumes_budget() -> None:
    surface = ModeDefinition(
        name="direct",
        is_default=True,
        allowed_tools=["allowed_tool"],
        terminals=["submit_task_completion"],
    )
    registry = ToolRegistry()
    registry.register(_AllowedTool())
    ctx = _StubContext(
        tool_registry=registry,
        cwd=Path("/tmp"),
        tool_call_limit=5,
        active_mode=surface,
    )

    async def _emit(_event):
        pass

    res = await execute_tool_call_streaming(
        ctx,  # type: ignore[arg-type]
        "allowed_tool",
        "tu-1",
        {},
        emit=_emit,
        emit_started=False,
    )

    assert not res.is_error
    assert ctx.tool_calls_used == 1
