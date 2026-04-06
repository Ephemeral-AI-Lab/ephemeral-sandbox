"""Higher-level system prompt assembly."""

from __future__ import annotations

from pathlib import Path

from config.paths import get_project_issue_file, get_project_pr_comments_file
from config.settings import Settings
from prompts.claudemd import load_claude_md_prompt
from prompts.system_prompt import build_system_prompt



def build_runtime_system_prompt(
    settings: Settings,
    *,
    cwd: str | Path,
    latest_user_prompt: str | None = None,
) -> str:
    """Build the runtime system prompt with project instructions and memory."""
    sections = [build_system_prompt(agent_system_prompt=settings.system_prompt, cwd=str(cwd))]

    if settings.fast_mode:
        sections.append(
            "# Session Mode\nFast mode is enabled. Prefer concise replies, minimal tool use, and quicker progress over exhaustive exploration."
        )

    sections.append(
        "# Reasoning Settings\n"
        f"- Effort: {settings.effort}\n"
        f"- Passes: {settings.passes}\n"
        "Adjust depth and iteration count to match these settings while still completing the task."
    )

    claude_md = load_claude_md_prompt(cwd)
    if claude_md:
        sections.append(claude_md)

    for title, path in (
        ("Issue Context", get_project_issue_file(cwd)),
        ("Pull Request Comments", get_project_pr_comments_file(cwd)),
    ):
        if path.exists():
            content = path.read_text(encoding="utf-8", errors="replace").strip()
            if content:
                sections.append(f"# {title}\n\n```md\n{content[:12000]}\n```")

    return "\n\n".join(section for section in sections if section.strip())


def build_agent_capabilities_prompt(
    toolkits: list["BaseToolkit"],
    has_background_tools: bool = False,
    bg_tool_names: list[str] | None = None,
) -> str:
    """Build the full toolkit and capability awareness section.

    Args:
        toolkits: Registered toolkits for behavioral guidance.
        has_background_tools: Whether background execution is available.
        bg_tool_names: Names of tools that support background execution.
    """
    from tools.base import BaseToolkit  # noqa: F811 — used for type only

    sections: list[str] = []

    # Toolkit instructions — only include toolkits that have behavioral guidance
    tk_sections = []
    for tk in toolkits:
        if tk.instructions:
            tk_sections.append(f"## {tk.name}\n{tk.instructions}")
    if tk_sections:
        sections.append("# Toolkit Instructions\n\n" + "\n\n".join(tk_sections))

    # Task note enforcement (when background tools are available)
    if has_background_tools:
        sections.append(build_task_note_prompt())

    return "\n\n".join(sections)


def build_task_note_prompt() -> str:
    """Build the system prompt section for the mandatory task_note field."""
    return (
        "# Tool Call Notes\n\n"
        "**Every tool call MUST include a `\"task_note\"` field (~20 words) "
        "describing what you are doing and why.** The call will be rejected without it. "
        "This note appears in logs and progress reports "
        "so you can recall context later.\n\n"
        "Example: `\"task_note\": \"running full pytest suite to verify auth changes before merge to main\"`\n"
    )


