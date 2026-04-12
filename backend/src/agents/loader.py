"""Agent definition loading from Markdown files with YAML frontmatter."""

from __future__ import annotations

import logging
import os
from pathlib import Path
from typing import Any

import yaml
from pydantic import ValidationError

from agents.types import AgentDefinition
from config.paths import get_config_dir

logger = logging.getLogger(__name__)


def _parse_frontmatter(content: str) -> tuple[dict[str, Any], str]:
    """Split a markdown file into (frontmatter dict, body)."""
    lines = content.splitlines()
    if not lines or lines[0].strip() != "---":
        return {}, content
    try:
        end = next(i for i, line in enumerate(lines[1:], start=1) if line.strip() == "---")
    except StopIteration:
        return {}, content
    try:
        fm = yaml.safe_load("\n".join(lines[1:end])) or {}
    except yaml.YAMLError:
        return {}, content
    if not isinstance(fm, dict):
        fm = {}
    body = "\n".join(lines[end + 1 :]).strip()
    return fm, body


def load_agents_dir(directory: Path) -> list[AgentDefinition]:
    """Load agent definitions from .md files in *directory*."""
    if not directory.is_dir():
        return []
    agents: list[AgentDefinition] = []
    for path in sorted(directory.glob("*.md")):
        try:
            fm, body = _parse_frontmatter(path.read_text(encoding="utf-8"))
            data = dict(fm)
            data.setdefault("name", path.stem)
            description = str(data.get("description") or f"Agent: {data['name']}")
            data["description"] = description.replace("\\n", "\n")
            if body:
                data["system_prompt"] = body
            data["source"] = "user"
            agents.append(AgentDefinition.model_validate(data))
        except ValidationError:
            logger.debug("Invalid agent definition in %s", path, exc_info=True)
        except Exception:
            logger.debug("Failed to load agent from %s", path, exc_info=True)
    return agents


def load_external_agents() -> list[AgentDefinition]:
    """Load all user-directory and plugin-provided agent definitions."""
    out: dict[str, AgentDefinition] = {}
    for defn in load_agents_dir(get_config_dir() / "agents"):
        out[defn.name] = defn
    try:
        from config.settings import load_settings
        from plugins.loader import load_plugins

        for plugin in load_plugins(load_settings(), os.getcwd()):
            if not plugin.enabled:
                continue
            for defn in getattr(plugin, "agents", []):
                if isinstance(defn, AgentDefinition):
                    out[defn.name] = defn
    except Exception:
        logger.debug("Failed to load plugin agent definitions", exc_info=True)
    return list(out.values())
