"""Instruction-only system prompt builder for EphemeralOS."""

from __future__ import annotations

def build_system_prompt(
    agent_system_prompt: str | None = None,
    env: object | None = None,
    cwd: str | None = None,
) -> str:
    """Build the instruction-only system prompt.

    Args:
        agent_system_prompt: Instructional prompt content to return.
        env: Deprecated, ignored. Kept for call-site compatibility.
        cwd: Deprecated, ignored. Kept for call-site compatibility.

    Returns:
        The assembled system prompt string.
    """
    del env, cwd
    return (agent_system_prompt or "").strip()
