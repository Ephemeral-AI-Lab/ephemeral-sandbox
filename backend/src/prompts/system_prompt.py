"""System prompt builder for EphemeralOS.

Assembles the system prompt from environment info and the agent's own prompt.
No base system prompt — behavior is fully driven by agent definitions,
tool schemas, and capability prompts.
"""

from __future__ import annotations

from prompts.environment import EnvironmentInfo, get_environment_info


def _format_environment_section(env: EnvironmentInfo) -> str:
    """Format the environment info section of the system prompt."""
    lines = [
        "# Environment",
        f"- OS: {env.os_name} {env.os_version}",
        f"- Architecture: {env.platform_machine}",
        f"- Shell: {env.shell}",
        f"- Working directory: {env.cwd}",
        f"- Date: {env.date}",
        f"- Python: {env.python_version}",
    ]

    if env.is_git_repo:
        git_line = "- Git: yes"
        if env.git_branch:
            git_line += f" (branch: {env.git_branch})"
        lines.append(git_line)

    return "\n".join(lines)


def build_system_prompt(
    agent_system_prompt: str | None = None,
    env: EnvironmentInfo | None = None,
    cwd: str | None = None,
) -> str:
    """Build the system prompt from the agent's own prompt + environment info.

    Args:
        agent_system_prompt: The agent's system prompt. If None, only environment info is returned.
        env: Pre-built EnvironmentInfo. If None, auto-detects.
        cwd: Working directory override (only used when env is None).

    Returns:
        The assembled system prompt string.
    """
    if env is None:
        env = get_environment_info(cwd=cwd)

    env_section = _format_environment_section(env)

    if agent_system_prompt:
        return f"{agent_system_prompt}\n\n{env_section}"

    return env_section
