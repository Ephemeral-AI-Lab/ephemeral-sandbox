"""Tests for prompts.runtime_prompt and background-related toolkit guidance."""

from __future__ import annotations

from prompts.runtime_prompt import build_background_lifecycle_prompt
from tools.builtins.background import make_background_toolkit
from tools.subagent import SubagentToolkit


def test_background_lifecycle_prompt_discourages_immediate_wait():
    prompt = build_background_lifecycle_prompt()

    assert "Do not make `wait_for_background_task` your immediate next move" in prompt
    assert "remaining foreground analysis or tool work first" in prompt


def test_subagent_toolkit_treats_spawned_workers_as_background():
    toolkit = SubagentToolkit()

    assert "workers always run in the background" in toolkit.instructions
    assert "Do not immediately block on the new task" in toolkit.instructions


def test_background_toolkit_says_wait_only_after_foreground_work():
    toolkit = make_background_toolkit(["run_subagent"])

    assert "do not immediately block on the new task unless it is the only blocker left" in toolkit.instructions
    assert "Use `wait_for_background_task` only when you are otherwise idle" in toolkit.instructions
