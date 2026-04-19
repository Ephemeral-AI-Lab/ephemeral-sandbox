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
    terminal_tools: set[str] | list[str] | None = None,
) -> str:
    """Build the full toolkit and capability awareness section.

    Args:
        toolkits: Registered toolkits for behavioral guidance.
        has_background_tools: Whether background execution is available.
        bg_tool_names: Names of tools that support background execution.
        terminal_tools: Tools that terminate the run immediately when called.
    """
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
    available_skills: list[dict[str, object]] = []
    skill_tool_names: list[str] = []
    background_lines: list[str] = []
    terminal_lines: list[str] = []
    for tk in toolkits:
        raw_skills = getattr(tk, "available_skills", None)
        tools = tk.list_tools()

        if tk.name == "skills":
            if not tools:
                continue
            skill_tool_names = [tool.name for tool in tools]
            if isinstance(raw_skills, list):
                for item in raw_skills:
                    if not isinstance(item, dict):
                        continue
                    name = str(item.get("name") or "").strip()
                    if not name:
                        continue
                    refs = item.get("references")
                    references = (
                        [str(ref).strip() for ref in refs if str(ref).strip()]
                        if isinstance(refs, list)
                        else []
                    )
                    available_skills.append(
                        {
                            "name": name,
                            "description": _compact_description(str(item.get("description") or "")),
                            "references": references,
                        }
                    )
            continue

        if tk.name == "background":
            if not tools:
                continue
            background_capable_tools = getattr(tk, "background_capable_tools", None)
            capable = []
            if isinstance(background_capable_tools, list):
                capable = [str(name).strip() for name in background_capable_tools if str(name).strip()]
            elif bg_tool_names:
                capable = [str(name).strip() for name in bg_tool_names if str(name).strip()]
            background_lines = [
                "Use background execution for long-running work when you can keep making foreground progress.",
            ]
            if capable:
                background_lines.append(
                    "Background-capable tools: " + ", ".join(f"`{name}`" for name in capable) + "."
                )
            background_lines.append(
                "Prefer foreground work or a single wait when blocked; call `check_background_progress` only when live status will change your next action."
            )
            background_lines.append(
                "`delivered`, `[COMPLETED]`, `[ALREADY_COMPLETED]`, and `[NO TASKS RUNNING]` are terminal signals; retire those task ids and act on the result instead of polling or waiting again."
            )
            background_lines.append(
                "For `run_subagent` results that say `Posted.`, background tools will only repeat the delivery envelope; use the relevant note/artifact reader next. In team-planner contexts, read current-task notes with `read_task_note(scope=\"own\", paths=None, task_note=\"Read posted scout notes\")` when exact scout paths are unclear, or `read_task_note(paths=[...])` for known scout scopes."
            )
            background_lines.append(
                "Cancel stale or low-value work promptly."
            )
            for idx, tool in enumerate(tools, start=1):
                tool_summary = getattr(tool, "short_description", None) or tool.description
                background_lines.append(
                    f"{idx}. {tool.name} - {_compact_description(tool_summary)}"
                )
            continue

        if isinstance(raw_skills, list):
            for item in raw_skills:
                if not isinstance(item, dict):
                    continue
                name = str(item.get("name") or "").strip()
                if not name:
                    continue
                refs = item.get("references")
                references = (
                    [str(ref).strip() for ref in refs if str(ref).strip()]
                    if isinstance(refs, list)
                    else []
                )
                available_skills.append(
                    {
                        "name": name,
                        "description": _compact_description(str(item.get("description") or "")),
                        "references": references,
                    }
                )
        if not tools:
            continue
        lines = [f"- {tk.name}: {_compact_description(tk.description)}"]
        for idx, tool in enumerate(tools, start=1):
            tool_summary = getattr(tool, "short_description", None) or tool.description
            lines.append(f"  {idx}. {tool.name} - {_compact_description(tool_summary)}")
        tk_sections.append("\n".join(lines))
    toolkit_section = ""
    if tk_sections:
        toolkit_intro = (
            "Use the following toolkits and tools that are available in this run.\n"
            "Treat this as the effective allowed tool surface for this run. "
            "Do not assume access to tools that are not listed here."
        )
        toolkit_section = (
            "<Toolkit Instructions>\n\n"
            + toolkit_intro
            + "\n\n"
            + "\n\n".join(tk_sections)
            + "\n\n</Toolkit Instructions>"
        )

    skills_section = ""
    if available_skills:
        deduped_skills: list[dict[str, str]] = []
        seen_skill_names: set[str] = set()
        for item in available_skills:
            if item["name"] in seen_skill_names:
                continue
            seen_skill_names.add(item["name"])
            deduped_skills.append(item)
        skill_lines: list[str] = []
        if "load_skill" in skill_tool_names:
            skill_lines.append(
                "Use `load_skill(skill_name)` when the task matches one of these skills."
            )
        if "load_skill_reference" in skill_tool_names:
            skill_lines.append(
                "Use `load_skill_reference(skill_name, reference_name)` for supplementary guidance, examples, and rubrics."
            )
            skill_lines.append(
                "Load the skill itself with `load_skill(...)` for the main playbook. "
                "Only call `load_skill_reference(...)` with one of the listed reference names; "
                "there is no `default` reference."
            )
        if skill_lines:
            skill_lines.append("")
        for item in deduped_skills:
            references = item.get("references")
            ref_names = [str(ref) for ref in references] if isinstance(references, list) else []
            if ref_names:
                ref_text = " References: " + ", ".join(f"`{ref}`" for ref in ref_names) + "."
            elif "load_skill_reference" in skill_tool_names:
                ref_text = " No references; use `load_skill(...)` only."
            else:
                ref_text = ""
            skill_lines.append(f"- {item['name']}: {item['description']}{ref_text}")
        skills_section = "<Available Skills>\n\n" + "\n".join(skill_lines) + "\n\n</Available Skills>"

    background_section = ""
    if background_lines and has_background_tools:
        background_section = "<Background Tasks>\n\n" + "\n".join(background_lines) + "\n\n</Background Tasks>"

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

    if toolkit_section:
        sections.append(toolkit_section)
    if skills_section:
        sections.append(skills_section)
    if background_section:
        sections.append(background_section)
    if terminal_section:
        sections.append(terminal_section)

    return "\n\n".join(sections)
