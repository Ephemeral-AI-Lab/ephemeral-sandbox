"""Tests for prompt.runtime_prompt and background-related tool guidance."""

from __future__ import annotations

from types import SimpleNamespace

from pydantic import BaseModel

from prompt.environment import EnvironmentInfo
from prompt.runtime_prompt import (
    build_runtime_context_message,
    build_runtime_system_prompt,
    build_termination_condition_prompt,
)
from tools.builtins.background import make_background_tools
from tools.core.base import BaseTool, ToolExecutionContextService, ToolRegistry, ToolResult
from tools.subagent import make_subagent_tools


class _EmptyInput(BaseModel):
    pass


class _DemoTool(BaseTool):
    name = "demo_tool"
    description = (
        "Inspect the current target and summarize the next safe action. "
        "Use only when the demo tool is active."
    )
    short_description = "Inspect the current target."
    input_model = _EmptyInput

    async def execute(self, arguments: BaseModel, context: ToolExecutionContextService) -> ToolResult:
        del arguments, context
        return ToolResult(output="ok")


def test_termination_condition_prompt_returns_empty_without_terminal_tools():
    prompt = build_termination_condition_prompt()

    assert prompt == ""
    assert "<Available Skills>" not in prompt
    assert "<Background Tasks>" not in prompt


def test_subagent_tools_expose_run_subagent_without_instruction_block():
    tools = make_subagent_tools()

    assert [tool.name for tool in tools] == ["run_subagent"]
    assert tools[0].short_description == "Spawn a subagent in the background."


def test_background_tools_expose_management_tools_without_instruction_block():
    tools = make_background_tools(["run_subagent"])

    assert [tool.name for tool in tools] == [
        "cancel_background_task",
        "check_background_task_result",
        "wait_background_tasks",
    ]


def test_termination_condition_prompt_omits_tool_call_notes_and_background_section():
    prompt = build_termination_condition_prompt(terminal_tools=["submit_plan"])

    assert "Tool Call Notes" not in prompt
    assert "<Background Tasks>" not in prompt
    assert "Background-capable tools: `run_subagent`." not in prompt
    assert "check_background_progress" not in prompt
    assert "<Termination Condition>" in prompt
    assert "- `submit_plan`" in prompt
    assert "WARNING: These are one-way exit tools." in prompt
    assert "Your lifecycle ends at that moment" in prompt
    assert "</Termination Condition>" in prompt


def test_termination_condition_prompt_only_renders_termination_condition():
    prompt = build_termination_condition_prompt(terminal_tools=["submit_plan"])

    assert "<Available Skills>" not in prompt
    assert "<Background Tasks>" not in prompt
    assert prompt.startswith("<Termination Condition>")
    assert "- `submit_plan`" in prompt


def test_tool_registry_remove_tools_filters_registered_tools():
    registry = ToolRegistry()
    registry.register(_DemoTool())

    registry.remove_tools(["demo_tool"])

    assert registry.get("demo_tool") is None


def test_tool_registry_restrict_to_tools_filters_registered_tools():
    registry = ToolRegistry()
    registry.register(_DemoTool())

    registry.restrict_to_tools(["missing_tool"])

    assert registry.get("demo_tool") is None


def test_runtime_context_message_contains_environment(monkeypatch):
    monkeypatch.setattr(
        "prompt.runtime_prompt.get_environment_info",
        lambda cwd=None: EnvironmentInfo(
            os_name="Linux",
            os_version="6.8.0",
            platform_machine="x86_64",
            shell="zsh",
            cwd=str(cwd or "/tmp/project"),
            home_dir="/home/user",
            date="2026-04-16",
            python_version="3.12.0",
            is_git_repo=True,
            git_branch="main",
            hostname="testhost",
        ),
    )

    prompt = build_runtime_context_message(cwd="/tmp/project")

    assert "# Environment" in prompt
    assert "Linux 6.8.0" in prompt
    assert "- Local host working directory: /tmp/project" in prompt
    assert "use the tool-reported cwd for sandbox commands" in prompt
    assert "- Working directory: /tmp/project" not in prompt
    assert "branch: main" in prompt


def test_runtime_system_prompt_omits_reasoning_settings():
    settings = SimpleNamespace(system_prompt="base prompt", fast_mode=False, effort="medium", passes=1)

    prompt = build_runtime_system_prompt(settings, cwd="/tmp/project")

    assert "base prompt" in prompt
    assert "# Reasoning Settings" not in prompt
    assert "- Effort:" not in prompt
    assert "- Passes:" not in prompt
