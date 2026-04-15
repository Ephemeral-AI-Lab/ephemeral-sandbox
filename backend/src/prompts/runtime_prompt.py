"""Higher-level system prompt assembly."""

from __future__ import annotations

import re
from pathlib import Path
from typing import Any

from config.paths import get_project_issue_file, get_project_pr_comments_file
from config.settings import Settings
from prompts.environment import EnvironmentInfo, get_environment_info
from prompts.system_prompt import build_system_prompt
from tools.core.base import BaseToolkit

__all__ = [
    "build_agent_capabilities_prompt",
    "build_runtime_context_message",
    "build_runtime_system_prompt",
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
    """Build the runtime instruction prompt for an agent run."""
    del latest_user_prompt
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


def _format_environment_context(env: EnvironmentInfo) -> str:
    """Render environment details as runtime user context."""
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


def build_runtime_context_message(*, cwd: str | Path) -> str:
    """Build runtime context to send as a user-role message."""
    sections = [_format_environment_context(get_environment_info(cwd=str(cwd)))]

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
    del has_background_tools, bg_tool_names
    sections: list[str] = []

    def _compact_description(text: str | None, *, max_words: int = 20) -> str:
        normalized = re.sub(r"\s+", " ", (text or "").strip())
        if not normalized:
            return "No description provided."
        sentence = normalized
        for match in re.finditer(r"[.!?]\s+", normalized):
            next_char = normalized[match.end() : match.end() + 1]
            if next_char and (next_char.isupper() or next_char in {"`", '"', "'"}):
                sentence = normalized[: match.start() + 1].strip()
                break
        words = sentence.split()
        if len(words) <= max_words:
            return sentence
        return " ".join(words[:max_words]).rstrip(" ,;:") + "..."

    tk_sections: list[str] = []
    available_skills: list[dict[str, str]] = []
    for tk in toolkits:
        raw_skills = getattr(tk, "available_skills", None)
        if isinstance(raw_skills, list):
            for item in raw_skills:
                if not isinstance(item, dict):
                    continue
                name = str(item.get("name") or "").strip()
                if not name:
                    continue
                available_skills.append(
                    {
                        "name": name,
                        "description": _compact_description(str(item.get("description") or "")),
                    }
                )
        tools = tk.list_tools()
        if not tools and not (tk.description or "").strip():
            continue
        lines = [f"- {tk.name}: {_compact_description(tk.description)}"]
        for idx, tool in enumerate(tools, start=1):
            tool_summary = getattr(tool, "short_description", None) or tool.description
            lines.append(f"  {idx}. {tool.name} - {_compact_description(tool_summary)}")
        tk_sections.append("\n".join(lines))
    if available_skills:
        deduped_skills: list[dict[str, str]] = []
        seen_skill_names: set[str] = set()
        for item in available_skills:
            if item["name"] in seen_skill_names:
                continue
            seen_skill_names.add(item["name"])
            deduped_skills.append(item)
        skill_lines = [f"- {item['name']}: {item['description']}" for item in deduped_skills]
        sections.append("<Available Skills>\n\n" + "\n".join(skill_lines) + "\n\n</Available Skills>")
    if tk_sections:
        sections.append("<Toolkit Instructions>\n\n" + "\n\n".join(tk_sections) + "\n\n</Toolkit Instructions>")

    return "\n\n".join(sections)
