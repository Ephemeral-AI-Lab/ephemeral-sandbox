"""Agent definition loading from Markdown files with YAML frontmatter."""

from __future__ import annotations

import logging
from collections.abc import Iterable
from pathlib import Path

from pydantic import ValidationError

from agents.types import AgentDefinition
from config.markdown import parse_markdown_frontmatter

logger = logging.getLogger(__name__)


def _load_agent_files(paths: Iterable[Path]) -> list[AgentDefinition]:
    agents: list[AgentDefinition] = []
    for path in sorted(paths):
        try:
            fm, body = parse_markdown_frontmatter(path.read_text(encoding="utf-8"))
            data = dict(fm)
            data.setdefault("name", path.stem)
            description = str(data.get("description") or f"Agent: {data['name']}")
            data["description"] = description.replace("\\n", "\n")
            if body:
                data["system_prompt"] = body
            agents.append(AgentDefinition.model_validate(data))
        except ValidationError:
            logger.debug("Invalid agent definition in %s", path, exc_info=True)
        except Exception:
            logger.debug("Failed to load agent from %s", path, exc_info=True)
    return agents


def load_agents_dir(directory: Path) -> list[AgentDefinition]:
    """Load agent definitions from .md files directly in *directory*."""
    if not directory.is_dir():
        return []
    return _load_agent_files(directory.glob("*.md"))


def load_agents_tree(directory: Path) -> list[AgentDefinition]:
    """Load agent definitions from all .md files under *directory*."""
    if not directory.is_dir():
        return []
    return _load_agent_files(directory.rglob("*.md"))
