"""Skill loading from bundled and user directories."""

from __future__ import annotations

from pathlib import Path

from config.paths import get_builtin_skills_dir
from skills.bundled import get_bundled_skills
from skills.core.registry import SkillRegistry
from skills.core.types import SkillDefinition


def get_user_skills_dir() -> Path:
    """Return the config-backed skills directory.

    Kept for compatibility with callers that need the on-disk skill root.
    """
    return get_builtin_skills_dir()


def load_skill_registry(cwd: str | Path | None = None) -> SkillRegistry:
    """Load config-backed skills."""
    registry = SkillRegistry()
    for skill in get_bundled_skills():
        registry.register(skill)
    return registry


def load_user_skills() -> list[SkillDefinition]:
    """Return user skill definitions.

    Skill definitions are now loaded from ``backend/config/skills`` only.
    """
    return []


def _parse_skill_markdown(default_name: str, content: str) -> tuple[str, str]:
    """Parse name and description from a skill markdown file with YAML frontmatter support."""
    name = default_name
    description = ""

    lines = content.splitlines()

    # Try YAML frontmatter first (--- ... ---)
    if lines and lines[0].strip() == "---":
        for i, line in enumerate(lines[1:], 1):
            if line.strip() == "---":
                # Parse frontmatter fields
                for fm_line in lines[1:i]:
                    fm_stripped = fm_line.strip()
                    if fm_stripped.startswith("name:"):
                        val = fm_stripped[5:].strip().strip("'\"")
                        if val:
                            name = val
                    elif fm_stripped.startswith("description:"):
                        val = fm_stripped[12:].strip().strip("'\"")
                        if val:
                            description = val
                break

    # Fallback: extract from headings and first paragraph
    if not description:
        for line in lines:
            stripped = line.strip()
            if stripped.startswith("# "):
                if not name or name == default_name:
                    name = stripped[2:].strip() or default_name
                continue
            if stripped and not stripped.startswith("---") and not stripped.startswith("#"):
                description = stripped[:200]
                break

    if not description:
        description = f"Skill: {name}"
    return name, description
