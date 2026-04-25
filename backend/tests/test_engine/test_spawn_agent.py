"""Tests for tool-only agent tool registration."""

from __future__ import annotations

from pathlib import Path
from types import SimpleNamespace
from typing import Any

import pytest
from pydantic import BaseModel

from agents.types import AgentDefinition
from engine.runtime.agent import _build_agent_tool_registry, finalize_tool_registry_and_prompt
from tools.core.base import BaseTool, ToolExecutionContext, ToolRegistry, ToolResult
from tools.core.factory import (
    ToolFactoryContext,
    _factories,
    create_tool,
    has_tool,
    register_tool_factory,
)


class _EmptyInput(BaseModel):
    pass


class _DummyTool(BaseTool):
    name = "dummy_tool"
    description = "Dummy tool."
    input_model = _EmptyInput

    async def execute(self, arguments: BaseModel, context: ToolExecutionContext) -> ToolResult:
        del arguments, context
        return ToolResult(output="ok")


class _CapturingTool(_DummyTool):
    name = "capturing_tool"
    captured_contexts: list[ToolFactoryContext] = []


@pytest.fixture(autouse=True)
def _isolate_tool_factories():
    original = dict(_factories)
    _factories.clear()
    _factories.update(original)
    _CapturingTool.captured_contexts.clear()
    yield
    _factories.clear()
    _factories.update(original)


def _make_config(cwd: str = "/tmp/project") -> SimpleNamespace:
    return SimpleNamespace(cwd=cwd)


def _make_agent_def(**overrides: Any) -> AgentDefinition:
    data: dict[str, Any] = {
        "name": "agent",
        "description": "Agent",
        "tools": [],
        "include_skills": False,
    }
    data.update(overrides)
    return AgentDefinition(**data)


def test_tool_registry_register_many_and_restrict_to_tools() -> None:
    registry = ToolRegistry()
    registry.register_many([_DummyTool(), _CapturingTool()])

    registry.restrict_to_tools(["capturing_tool"])

    assert registry.get("dummy_tool") is None
    assert registry.get("capturing_tool") is not None


def test_tool_factory_creates_named_tool() -> None:
    register_tool_factory("dummy_tool", lambda ctx: _DummyTool())

    tool = create_tool("dummy_tool", ToolFactoryContext())

    assert has_tool("dummy_tool")
    assert tool.name == "dummy_tool"


def test_build_agent_tool_registry_registers_explicit_tools() -> None:
    register_tool_factory("dummy_tool", lambda ctx: _DummyTool())
    agent_def = _make_agent_def(tools=["dummy_tool"])

    registry = _build_agent_tool_registry(_make_config(), agent_def, None, "agent")

    assert registry.get("dummy_tool") is not None


def test_tool_factory_context_carries_agent_metadata() -> None:
    def factory(ctx: ToolFactoryContext) -> BaseTool:
        _CapturingTool.captured_contexts.append(ctx)
        return _CapturingTool()

    register_tool_factory("capturing_tool", factory)
    agent_def = _make_agent_def(
        name="my-agent",
        role="developer",
        tools=["capturing_tool"],
    )

    _build_agent_tool_registry(
        _make_config(cwd="/repo"),
        agent_def,
        "sb-123",
        "my-agent",
    )

    assert len(_CapturingTool.captured_contexts) == 1
    captured = _CapturingTool.captured_contexts[0]
    assert captured.metadata["agent_name"] == "my-agent"
    assert captured.metadata["role"] == "developer"
    assert captured.metadata["cwd"] == "/repo"
    assert captured.metadata["sandbox_id"] == "sb-123"


def test_build_agent_tool_registry_skips_unknown_tools() -> None:
    agent_def = _make_agent_def(tools=["missing_tool"])

    registry = _build_agent_tool_registry(_make_config(), agent_def, None, "agent")

    assert registry.list_tools() == []


def test_finalize_adds_background_management_tools_for_background_capable_tool() -> None:
    from tools.subagent.run_subagent_tool import run_subagent

    registry = ToolRegistry()
    registry.register(run_subagent)

    _, has_background = finalize_tool_registry_and_prompt(
        registry,
        "base",
        can_spawn_subagents=True,
    )

    assert has_background is True
    assert registry.get("check_background_progress") is not None
    assert registry.get("wait_for_background_task") is not None
    assert registry.get("cancel_background_task") is not None


def test_default_sandbox_agent_registers_daytona_tools() -> None:
    registry = _build_agent_tool_registry(
        _make_config(cwd=str(Path("/tmp/project"))),
        None,
        "sb-123",
        "default",
    )

    assert registry.get("daytona_read_file") is not None
    assert registry.get("daytona_shell") is not None
