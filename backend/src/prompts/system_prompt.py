"""Instruction-only system prompt builder for EphemeralOS."""

from __future__ import annotations


def build_system_prompt(agent_system_prompt: str | None = None) -> str:
    """Build the instruction-only system prompt.

    Args:
        agent_system_prompt: Instructional prompt content to return.

    Returns:
        The assembled system prompt string.
    """
    return (agent_system_prompt or "").strip()
