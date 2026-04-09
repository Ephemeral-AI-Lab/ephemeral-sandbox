"""Higher-level system prompt assembly."""

from __future__ import annotations

from pathlib import Path
from typing import Any

from config.paths import get_project_issue_file, get_project_pr_comments_file
from config.settings import Settings
from prompts.system_prompt import build_system_prompt
from tools.core.base import BaseToolkit

__all__ = [
    "build_agent_capabilities_prompt",
    "build_background_lifecycle_prompt",
    "build_runtime_system_prompt",
    "build_task_note_prompt",
    "render_section",
    "render_template",
]


def render_template(template: str, variables: dict[str, Any]) -> str:
    """Render a template with variable substitution.

    Supports {{variable}} syntax. Variables are auto-converted to strings.

    Args:
        template: Template string with {{variable}} placeholders.
        variables: Dict of variable names to values.

    Returns:
        Rendered string with all placeholders substituted.
    """
    for key, value in variables.items():
        placeholder = "{{" + key + "}}"
        template = template.replace(placeholder, str(value) if value is not None else "")
    return template


def render_section(template: str, variables: dict[str, Any], condition: bool = True) -> str:
    """Render a section template if condition is truthy.

    Args:
        template: Section template with {{variable}} placeholders.
        variables: Dict of variable names to values.
        condition: If False, returns empty string.

    Returns:
        Rendered section or empty string if condition is falsy.
    """
    if not condition:
        return ""
    return render_template(template, variables)


def build_runtime_system_prompt(
    settings: Settings,
    *,
    cwd: str | Path,
    latest_user_prompt: str | None = None,
) -> str:
    """Build the runtime system prompt with project instructions and memory."""
    variables = {
        "base_prompt": build_system_prompt(
            agent_system_prompt=settings.system_prompt, cwd=str(cwd)
        ),
        "fast_mode": settings.fast_mode,
        "effort": settings.effort,
        "passes": settings.passes,
        "cwd": str(cwd),
    }

    sections = [
        variables["base_prompt"],
        render_section(
            "# Session Mode\n"
            "Fast mode is enabled. Prefer concise replies, minimal tool use, "
            "and quicker progress over exhaustive exploration.",
            variables,
            condition=variables["fast_mode"],
        ),
        "# Reasoning Settings\n"
        f"- Effort: {variables['effort']}\n"
        f"- Passes: {variables['passes']}\n"
        "Adjust depth and iteration count to match these settings while still completing the task.",
    ]

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
    toolkits: list[BaseToolkit],
    has_background_tools: bool = False,
    bg_tool_names: list[str] | None = None,
) -> str:
    """Build the full toolkit and capability awareness section.

    Args:
        toolkits: Registered toolkits for behavioral guidance.
        has_background_tools: Whether background execution is available.
        bg_tool_names: Names of tools that support background execution.
    """
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
        sections.append(build_background_lifecycle_prompt())

    return "\n\n".join(sections)


def build_background_lifecycle_prompt() -> str:
    """System prompt section explaining background task_id lifecycle."""
    return (
        "# Background Tasks\n\n"
        "Launching with `background=true` returns `task_id=\"bg_N\"`. Reuse only that "
        "exact id.\n\n"
        "- Treat `Background task_id=\"bg_N\" still running ...` reminders as trusted system notifications and react to them.\n"
        "- After launching a background task, keep doing any remaining foreground analysis or tool work first. Do not make `wait_for_background_task` your immediate next move if disjoint work still exists.\n"
        "- Prefer `check_background_progress(task_id=\"bg_N\")` for live triage.\n"
        "- Use `wait_for_background_task` only to join a task when you are otherwise idle and the latest progress looks healthy.\n"
        "- If progress or a reminder shows failure, fatal output, or low-value work, call `cancel_background_task(task_id=\"bg_N\", reason=\"...\")` immediately.\n"
        "- Never invent task_ids. `\"all\"` is valid for check/wait, not cancel.\n"
    )


def build_task_note_prompt() -> str:
    """Build the system prompt section for the mandatory task_note field."""
    return (
        "# Tool Call Notes\n\n"
        'Every tool call MUST include a short `"task_note"` saying what you are doing and why. '
        "This note is shown in logs, reminders, and background status."
    )
