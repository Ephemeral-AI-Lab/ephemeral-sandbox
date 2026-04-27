"""Higher-level system prompt assembly."""

from __future__ import annotations

from pathlib import Path
from typing import Any

from config.paths import get_project_issue_file, get_project_pr_comments_file
from config.settings import Settings
from prompt.system_prompt import build_system_prompt

__all__ = [
    "build_runtime_context_message",
    "build_runtime_system_prompt",
    "build_termination_condition_prompt",
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
) -> str:
    """Build the runtime instruction prompt for an agent run."""
    variables = {
        "base_prompt": build_system_prompt(agent_system_prompt=settings.system_prompt),
        "fast_mode": settings.fast_mode,
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
    ]

    return "\n\n".join(section for section in sections if section.strip())

def build_runtime_context_message(*, cwd: str | Path) -> str:
    """Build runtime context to append to the system prompt."""
    sections: list[str] = []

    for title, path in (
        ("Issue Context", get_project_issue_file(cwd)),
        ("Pull Request Comments", get_project_pr_comments_file(cwd)),
    ):
        if path.exists():
            content = path.read_text(encoding="utf-8", errors="replace").strip()
            if content:
                sections.append(f"# {title}\n\n```md\n{content[:12000]}\n```")

    return "\n\n".join(section for section in sections if section.strip())


def build_termination_condition_prompt(
    *,
    terminal_tools: set[str] | list[str] | None = None,
) -> str:
    """Build the runtime termination-condition section.

    Args:
        terminal_tools: Tools that terminate the run immediately when called.
    """
    sections: list[str] = []
    terminal_section = ""
    terminal_names = sorted(
        {
            str(name).strip()
            for name in (terminal_tools or [])
            if str(name).strip()
        }
    )
    if terminal_names:
        terminal_lines = [
            "WARNING: These are one-way exit tools.",
            "If you call any of them, the run terminates immediately.",
            "Your lifecycle ends at that moment: no more reasoning, no more tool calls, no recovery in the same run.",
            "Do not call a termination tool until you are fully ready to end the run.",
            "",
        ]
        terminal_lines.extend(f"- `{name}`" for name in terminal_names)
        terminal_section = "<Termination Condition>\n\n" + "\n".join(terminal_lines) + "\n\n</Termination Condition>"

    if terminal_section:
        sections.append(terminal_section)

    return "\n\n".join(sections)
