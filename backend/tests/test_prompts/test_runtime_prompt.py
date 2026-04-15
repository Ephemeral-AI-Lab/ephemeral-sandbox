"""Tests for prompts.runtime_prompt and background-related toolkit guidance."""

from __future__ import annotations

from types import SimpleNamespace

from pydantic import BaseModel

from prompts.environment import EnvironmentInfo
from prompts.runtime_prompt import (
    build_agent_capabilities_prompt,
    build_runtime_context_message,
    build_runtime_system_prompt,
)
from tools.core.base import BaseTool, BaseToolkit, ToolExecutionContext, ToolResult
from tools.builtins.background import make_background_toolkit
from tools.subagent import SubagentToolkit


class _EmptyInput(BaseModel):
    pass


class _DemoTool(BaseTool):
    name = "demo_tool"
    description = (
        "Inspect the current target and summarize the next safe action. "
        "Use only when the demo toolkit is active."
    )
    short_description = "Inspect the current target."
    input_model = _EmptyInput

    async def execute(self, arguments: BaseModel, context: ToolExecutionContext) -> ToolResult:
        del arguments, context
        return ToolResult(output="ok")


def test_agent_capabilities_prompt_uses_compact_toolkit_format():
    prompt = build_agent_capabilities_prompt(
        [
            BaseToolkit(
                name="demo",
                description="Demo toolkit for bounded inspection and repair work.",
                tools=[_DemoTool()],
                instructions="This verbose guidance should not be rendered.",
            )
        ]
    )

    assert prompt.startswith("<Toolkit Instructions>")
    assert "</Toolkit Instructions>" in prompt
    assert "- demo: Demo toolkit for bounded inspection and repair work." in prompt
    assert "1. demo_tool - Inspect the current target." in prompt
    assert "This verbose guidance should not be rendered." not in prompt


def test_subagent_toolkit_treats_spawned_workers_as_background():
    toolkit = SubagentToolkit()

    assert "workers always run in the background" in toolkit.instructions
    assert "Do not immediately block on the new task" in toolkit.instructions
    assert "inspect that exact `task_id` with `check_background_progress` before the first `wait_for_background_task`" in toolkit.instructions
    assert toolkit.get("run_subagent").short_description == "Spawn a subagent in the background."


def test_background_toolkit_says_wait_only_after_foreground_work():
    toolkit = make_background_toolkit(["run_subagent"])

    assert "do not immediately block on the new task unless it is the only blocker left" in toolkit.instructions
    assert "Use `wait_for_background_task` only when you are otherwise idle" in toolkit.instructions


def test_agent_capabilities_prompt_omits_tool_call_notes_and_background_tasks():
    prompt = build_agent_capabilities_prompt(
        [SubagentToolkit()],
        has_background_tools=True,
        bg_tool_names=["run_subagent"],
    )

    assert "Tool Call Notes" not in prompt
    assert "Background Tasks" not in prompt


def test_agent_capabilities_prompt_prefers_short_description_over_description():
    class _LongTool(BaseTool):
        name = "long_tool"
        description = (
            "This tool has a single sentence description that keeps going past the preferred "
            "display width so the prompt builder should trim it to a shorter summary for the "
            "toolkit instructions block without copying the whole annotation verbatim"
        )
        short_description = "Use the concise summary instead."
        input_model = _EmptyInput

        async def execute(self, arguments: BaseModel, context: ToolExecutionContext) -> ToolResult:
            del arguments, context
            return ToolResult(output="ok")

    prompt = build_agent_capabilities_prompt(
        [BaseToolkit(name="demo", description="Demo toolkit", tools=[_LongTool()])]
    )

    assert "1. long_tool - Use the concise summary instead." in prompt
    assert "display width" not in prompt


def test_runtime_context_message_contains_environment(monkeypatch):
    monkeypatch.setattr(
        "prompts.runtime_prompt.get_environment_info",
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
    assert "branch: main" in prompt


def test_runtime_system_prompt_omits_reasoning_settings():
    settings = SimpleNamespace(system_prompt="base prompt", fast_mode=False, effort="medium", passes=1)

    prompt = build_runtime_system_prompt(settings, cwd="/tmp/project")

    assert "base prompt" in prompt
    assert "# Reasoning Settings" not in prompt
    assert "- Effort:" not in prompt
    assert "- Passes:" not in prompt
