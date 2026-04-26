"""Tests for tool-only agent tool registration."""

from __future__ import annotations

from pathlib import Path
from types import SimpleNamespace
from typing import Any

import pytest
from pydantic import BaseModel

from agents.types import AgentDefinition, ModeDefinition
from engine.runtime.agent import _build_agent_tool_registry, finalize_tool_registry_and_prompt
from tools.core.base import BaseTool, ToolExecutionContextService, ToolRegistry, ToolResult
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

    async def execute(self, arguments: BaseModel, context: ToolExecutionContextService) -> ToolResult:
        del arguments, context
        return ToolResult(output="ok")


class _CapturingTool(_DummyTool):
    name = "capturing_tool"
    captured_contexts: list[ToolFactoryContext] = []


class _TerminalTool(_DummyTool):
    name = "terminal_tool"
    is_terminal_tool = True


class _BackgroundCapableTool(_DummyTool):
    name = "bg_capable_tool"
    background = "optional"


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
    allowed_tools = overrides.pop("allowed_tools", [])
    data: dict[str, Any] = {
        "name": "agent",
        "description": "Agent",
        "modes": [
            ModeDefinition(
                name="direct",
                is_default=True,
                allowed_tools=allowed_tools,
                terminals=["submit_task_completion"],
            )
        ],
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
    agent_def = _make_agent_def(allowed_tools=["dummy_tool"])

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
        allowed_tools=["capturing_tool"],
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
    agent_def = _make_agent_def(allowed_tools=["missing_tool"])

    registry = _build_agent_tool_registry(_make_config(), agent_def, None, "agent")

    # The unknown tool is skipped (with a warning). The auto-synthesized
    # default-mode terminal still resolves via the global factory.
    assert registry.get("missing_tool") is None


def test_finalize_adds_background_management_tools_for_background_capable_tool() -> None:
    registry = ToolRegistry()
    registry.register(_BackgroundCapableTool())

    _, has_background = finalize_tool_registry_and_prompt(
        registry,
        "base",
        agent_type="agent",
    )

    assert has_background is True
    assert registry.get("wait_background_tasks") is not None
    assert registry.get("cancel_background_task") is not None


def test_run_subagent_factory_preserves_always_background_policy() -> None:
    tool = create_tool("run_subagent", ToolFactoryContext())

    assert tool.background == "always"
    assert tool.task_type == "subagent"

    registry = ToolRegistry()
    registry.register(tool)

    _, has_background = finalize_tool_registry_and_prompt(
        registry,
        "base",
        agent_type="agent",
    )

    assert has_background is True
    assert registry.get("wait_background_tasks") is not None
    assert registry.get("check_background_task_result") is not None


def test_finalize_skips_background_management_tools_for_subagent() -> None:
    registry = ToolRegistry()
    registry.register(_BackgroundCapableTool())

    _, has_background = finalize_tool_registry_and_prompt(
        registry,
        "base",
        agent_type="subagent",
    )

    assert has_background is False
    assert registry.get("wait_background_tasks") is None
    assert registry.get("cancel_background_task") is None


def test_finalize_derives_terminal_tool_guidance_from_registry() -> None:
    registry = ToolRegistry()
    registry.register(_TerminalTool())

    prompt, _ = finalize_tool_registry_and_prompt(
        registry,
        "base",
        agent_type="agent",
    )

    assert prompt.startswith("base")
    assert "<Termination Condition>" in prompt
    assert "- `terminal_tool`" in prompt


def test_build_agent_tool_registry_does_not_register_skill_tools(monkeypatch) -> None:
    calls: list[list[str] | None] = []

    monkeypatch.setattr(
        "skills.core.loader.load_skill_registry",
        lambda cwd: object(),
    )

    def _fake_make_skills_tools(_registry: object, skill_filter: list[str] | None = None) -> list[BaseTool]:
        calls.append(skill_filter)
        return [_DummyTool()]

    monkeypatch.setattr(
        "tools.builtins.skills.make_skills_tools",
        _fake_make_skills_tools,
    )

    no_skills_registry = _build_agent_tool_registry(
        _make_config(),
        _make_agent_def(),
        None,
        "agent",
    )
    declared_skills_registry = _build_agent_tool_registry(
        _make_config(),
        _make_agent_def(skills=["demo-skill"]),
        None,
        "agent",
    )

    assert no_skills_registry.get("dummy_tool") is None
    assert declared_skills_registry.get("dummy_tool") is None
    assert calls == []


def test_default_sandbox_agent_registers_daytona_tools() -> None:
    registry = _build_agent_tool_registry(
        _make_config(cwd=str(Path("/tmp/project"))),
        None,
        "sb-123",
        "default",
    )

    assert registry.get("read_file") is not None
    assert registry.get("shell") is not None
