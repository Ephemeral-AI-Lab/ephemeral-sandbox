"""Tests for prompts.system_prompt."""

from __future__ import annotations

from prompts.system_prompt import build_system_prompt


def test_build_system_prompt_returns_instruction_text_only():
    prompt = build_system_prompt(agent_system_prompt="You are a helpful bot.")

    assert prompt == "You are a helpful bot."
    assert "# Environment" not in prompt


def test_build_system_prompt_empty_when_no_agent_prompt():
    assert build_system_prompt() == ""
