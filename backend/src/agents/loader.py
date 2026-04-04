"""Agent definition loading from YAML/Markdown files and unified lookup."""

from __future__ import annotations

import logging
from pathlib import Path
from typing import Any

import yaml

from ephemeralos.agents.types import (
    EFFORT_LEVELS,
    AgentDefinition,
    parse_positive_int,
    parse_str_list,
)
from ephemeralos.config.paths import get_config_dir

logger = logging.getLogger(__name__)


def _parse_agent_frontmatter(content: str) -> tuple[dict[str, Any], str]:
    """Parse YAML frontmatter from a markdown file."""
    frontmatter: dict[str, Any] = {}
    body = content
    lines = content.splitlines()
    if not lines or lines[0].strip() != "---":
        return frontmatter, body
    end_index: int | None = None
    for i, line in enumerate(lines[1:], start=1):
        if line.strip() == "---":
            end_index = i
            break
    if end_index is None:
        return frontmatter, body
    fm_text = "\n".join(lines[1:end_index])
    try:
        parsed = yaml.safe_load(fm_text)
        if isinstance(parsed, dict):
            frontmatter = parsed
    except yaml.YAMLError:
        for fm_line in lines[1:end_index]:
            if ":" in fm_line:
                key, _, value = fm_line.partition(":")
                frontmatter[key.strip()] = value.strip().strip("'\"")
    body = "\n".join(lines[end_index + 1:]).strip()
    return frontmatter, body


def load_agents_dir(directory: Path) -> list[AgentDefinition]:
    """Load agent definitions from .md files in *directory*."""
    agents: list[AgentDefinition] = []
    if not directory.is_dir():
        return agents
    for path in sorted(directory.glob("*.md")):
        try:
            content = path.read_text(encoding="utf-8")
            frontmatter, body = _parse_agent_frontmatter(content)
            name = str(frontmatter.get("name", "")).strip() or path.stem
            description = str(frontmatter.get("description", "")).strip()
            if not description:
                description = f"Agent: {name}"
            description = description.replace("\\n", "\n")

            model_raw = frontmatter.get("model")
            model: str | None = None
            if isinstance(model_raw, str) and model_raw.strip():
                trimmed = model_raw.strip()
                model = "inherit" if trimmed.lower() == "inherit" else trimmed

            effort_raw = frontmatter.get("effort")
            effort: str | int | None = None
            if effort_raw is not None:
                if isinstance(effort_raw, int):
                    effort = effort_raw if effort_raw > 0 else None
                elif isinstance(effort_raw, str) and effort_raw in EFFORT_LEVELS:
                    effort = effort_raw

            max_turns = parse_positive_int(frontmatter.get("maxTurns", frontmatter.get("max_turns")))
            skills = parse_str_list(frontmatter.get("skills")) or []
            toolkits = parse_str_list(frontmatter.get("toolkits")) or []

            hooks_raw = frontmatter.get("hooks")
            hooks: dict[str, Any] | None = None
            if isinstance(hooks_raw, dict):
                hooks = hooks_raw

            bg_raw = frontmatter.get("background")
            background = bg_raw is True or bg_raw == "true"

            ip_raw = frontmatter.get("initialPrompt", frontmatter.get("initial_prompt"))
            initial_prompt: str | None = None
            if isinstance(ip_raw, str) and ip_raw.strip():
                initial_prompt = ip_raw

            ocm_raw = frontmatter.get("omitClaudeMd", frontmatter.get("omit_claude_md"))
            omit_claude_md = ocm_raw is True or ocm_raw == "true"

            csr_raw = frontmatter.get("criticalSystemReminder", frontmatter.get("critical_system_reminder"))
            critical_system_reminder: str | None = None
            if isinstance(csr_raw, str) and csr_raw.strip():
                critical_system_reminder = csr_raw

            permissions: list[str] = []
            raw_perms = frontmatter.get("permissions", "")
            if raw_perms:
                permissions = [p.strip() for p in str(raw_perms).split(",") if p.strip()]

            agents.append(
                AgentDefinition(
                    name=name, description=description, system_prompt=body or None,
                    model=model,
                    effort=effort, max_turns=max_turns,
                    skills=skills, toolkits=toolkits,
                    hooks=hooks, background=background,
                    initial_prompt=initial_prompt,
                    omit_claude_md=omit_claude_md, critical_system_reminder=critical_system_reminder,
                    permissions=permissions,
                    filename=path.stem, base_dir=str(directory),
                    subagent_type=str(frontmatter.get("subagent_type", name)),
                    source="user",
                )
            )
        except Exception:
            logger.debug("Failed to parse agent from %s", path, exc_info=True)
            continue
    return agents


def _get_user_agents_dir() -> Path:
    return get_config_dir() / "agents"


def get_all_agent_definitions() -> list[AgentDefinition]:
    """Return all agent definitions: built-in + user + plugin."""
    from ephemeralos.agents.builtins import get_builtin_agent_definitions  # noqa: PLC0415

    agent_map: dict[str, AgentDefinition] = {}
    for agent in get_builtin_agent_definitions():
        agent_map[agent.name] = agent
    for agent in load_agents_dir(_get_user_agents_dir()):
        agent_map[agent.name] = agent
    try:
        from ephemeralos.plugins.loader import load_plugins  # noqa: PLC0415
        from ephemeralos.config.settings import load_settings  # noqa: PLC0415
        import os  # noqa: PLC0415
        settings = load_settings()
        cwd = os.getcwd()
        for plugin in load_plugins(settings, cwd):
            if not plugin.enabled:
                continue
            for agent_def in getattr(plugin, "agents", []):
                if isinstance(agent_def, AgentDefinition):
                    agent_map[agent_def.name] = agent_def
    except Exception:
        pass
    return list(agent_map.values())


def get_agent_definition(name: str) -> AgentDefinition | None:
    """Return the agent definition for *name*, or None if not found."""
    from ephemeralos.agents.registry import get_definition  # noqa: PLC0415
    defn = get_definition(name)
    if defn is not None:
        return defn
    for agent in get_all_agent_definitions():
        if agent.name == name:
            return agent
    return None
