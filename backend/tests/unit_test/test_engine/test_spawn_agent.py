"""Tests for tool-only agent tool registration."""

from __future__ import annotations

import threading
from types import SimpleNamespace
from typing import Any

import pytest
from pydantic import BaseModel

from agents import AgentDefinition, AgentRole
from engine.agent.factory import (
    _attach_default_notification_rules,
    _build_agent_tool_registry,
    _build_sandbox_context_preparers,
    _finalize_tool_registry_and_prompt,
)
from tools._framework.core.base import BaseTool, ToolExecutionContextService, ToolResult
from tools._framework.core.registry import ToolRegistry
from tools.sandbox._lib.context import SANDBOX_CONTEXT
from tools._framework.factory import (
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

    async def execute(
        self, arguments: BaseModel, context: ToolExecutionContextService
    ) -> ToolResult:
        del arguments, context
        return ToolResult(output="ok")


class _CapturingTool(_DummyTool):
    name = "capturing_tool"
    captured_contexts: list[ToolFactoryContext] = []


class _TerminalTool(_DummyTool):
    name = "terminal_tool"
    is_terminal_tool = True


class _ExecCommandTool(_DummyTool):
    name = "exec_command"


class _ShellTool(_DummyTool):
    name = "shell"


class _SandboxContextTool(_DummyTool):
    name = "sandbox_context_tool"
    context_requirements = (SANDBOX_CONTEXT,)


@pytest.fixture(autouse=True)
def _isolate_tool_factories(monkeypatch: pytest.MonkeyPatch):
    original = dict(_factories)
    _factories.clear()
    _factories.update(original)
    _CapturingTool.captured_contexts.clear()
    from sandbox.provider import registry as provider_registry

    monkeypatch.setattr(provider_registry, "_ADAPTERS", {}, raising=False)
    monkeypatch.setattr(provider_registry, "_DEFAULT", None, raising=False)
    monkeypatch.setattr(provider_registry, "_LOCK", threading.Lock(), raising=False)
    yield
    _factories.clear()
    _factories.update(original)


def _make_config(cwd: str = "/tmp/project") -> SimpleNamespace:
    return SimpleNamespace(cwd=cwd)


def _make_agent_def(**overrides: Any) -> AgentDefinition:
    data: dict[str, Any] = {
        "name": "agent",
        "description": "Agent",
        "allowed_tools": overrides.pop("allowed_tools", []),
        "terminals": ["submit_generator_outcome"],
        "tool_call_limit": 10,
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
        role=AgentRole.GENERATOR,
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
    assert captured.metadata["role"] == "generator"
    assert captured.metadata["cwd"] == "/repo"
    assert captured.metadata["sandbox_id"] == "sb-123"


def test_build_agent_tool_registry_skips_unknown_tools() -> None:
    agent_def = _make_agent_def(allowed_tools=["missing_tool"])

    registry = _build_agent_tool_registry(_make_config(), agent_def, None, "agent")

    # The unknown tool is skipped (with a warning). The auto-synthesized
    # default-mode terminal still resolves via the global factory.
    assert registry.get("missing_tool") is None


def test_finalize_does_not_enable_background_manager_for_ordinary_tools() -> None:
    registry = ToolRegistry()
    registry.register(_DummyTool())
    registry.register(_TerminalTool())

    _, has_background = _finalize_tool_registry_and_prompt(
        registry,
        "base",
        agent_type="agent",
    )

    assert has_background is False
    assert registry.get("wait_background_tasks") is None
    assert registry.get("check_background_task_result") is None
    assert registry.get("cancel_background_task") is None


def test_finalize_enables_background_manager_for_pty_session_tools() -> None:
    registry = ToolRegistry()
    registry.register(_ExecCommandTool())
    registry.register(_TerminalTool())

    _, has_background = _finalize_tool_registry_and_prompt(
        registry,
        "base",
        agent_type="agent",
    )

    assert has_background is True
    assert registry.get("wait_background_tasks") is None
    assert registry.get("check_background_task_result") is None
    assert registry.get("cancel_background_task") is None


def test_finalize_registers_background_controls_for_generic_shell() -> None:
    registry = ToolRegistry()
    registry.register(_ShellTool())
    registry.register(_TerminalTool())

    _, has_background = _finalize_tool_registry_and_prompt(
        registry,
        "base",
        agent_type="agent",
    )

    assert has_background is True
    assert registry.get("check_background_task_result") is not None
    assert registry.get("cancel_background_task") is not None
    assert registry.get("wait_background_tasks") is None
    assert registry.get("check_subagent_progress") is None
    assert registry.get("cancel_subagent") is None


def test_run_subagent_factory_uses_typed_background_policy() -> None:
    tool = create_tool("run_subagent", ToolFactoryContext())

    assert tool.task_type == "subagent"

    registry = ToolRegistry()
    registry.register(tool)
    registry.register(_TerminalTool())

    _, has_background = _finalize_tool_registry_and_prompt(
        registry,
        "base",
        agent_type="agent",
    )

    assert has_background is True
    assert registry.get("check_subagent_progress") is not None
    assert registry.get("cancel_subagent") is not None
    assert registry.get("wait_background_tasks") is None
    assert registry.get("check_background_task_result") is None


def test_attach_default_notification_rules_adds_budget_tiers_once() -> None:
    rules = []

    _attach_default_notification_rules(rules)
    _attach_default_notification_rules(rules)

    assert [rule.name for rule in rules] == [
        "tool_call_budget_75_percent",
        "tool_call_budget_100_percent",
        "tool_call_budget_125_percent",
        "terminal_call_reminder",
    ]


def test_finalize_skips_background_management_tools_for_subagent() -> None:
    registry = ToolRegistry()
    registry.register(create_tool("run_subagent", ToolFactoryContext()))
    registry.register(_TerminalTool())

    _, has_background = _finalize_tool_registry_and_prompt(
        registry,
        "base",
        agent_type="subagent",
    )

    assert has_background is False
    assert registry.get("wait_background_tasks") is None
    assert registry.get("cancel_background_task") is None
    assert registry.get("check_subagent_progress") is None
    assert registry.get("cancel_subagent") is None


def test_finalize_derives_terminal_tool_guidance_from_registry() -> None:
    registry = ToolRegistry()
    registry.register(_TerminalTool())

    prompt, _ = _finalize_tool_registry_and_prompt(
        registry,
        "base",
        agent_type="agent",
    )

    assert prompt.startswith("base")
    assert "<Termination Condition>" in prompt
    assert "- `terminal_tool`" in prompt


def test_build_agent_tool_registry_skips_load_skill_reference_when_not_requested() -> None:
    """Skill tool only appears when the agent's allowed_tools requests it."""
    registry = _build_agent_tool_registry(
        _make_config(),
        _make_agent_def(),
        None,
        "agent",
    )

    assert registry.get("load_skill_reference") is None


def test_context_preparers_not_added_without_declared_requirement() -> None:
    registry = ToolRegistry()
    registry.register(_DummyTool())

    assert _build_sandbox_context_preparers(registry, "sb-123") == []


def test_context_preparers_follow_tool_declared_requirement() -> None:
    from sandbox.provider.daytona.bootstrap import bootstrap_daytona_provider

    bootstrap_daytona_provider()
    registry = ToolRegistry()
    registry.register(_SandboxContextTool())

    preparers = _build_sandbox_context_preparers(registry, "sb-123")

    assert len(preparers) == 1
    assert type(preparers[0]).__name__ == "DaytonaContextPreparer"
