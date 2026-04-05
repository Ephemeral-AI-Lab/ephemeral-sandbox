"""Bundled skill definitions loaded from directory-based skills.

Each skill is a subdirectory of ``content/`` containing a ``SKILL.md``
file with optional YAML frontmatter (``name``, ``description``).
Legacy flat ``.md`` files in ``content/`` are also supported.
"""

from __future__ import annotations

from pathlib import Path

from ephemeralos.skills.types import SkillDefinition

_CONTENT_DIR = Path(__file__).parent / "content"


def get_bundled_skills() -> list[SkillDefinition]:
    """Load all bundled skills from the content/ directory."""
    skills: list[SkillDefinition] = []
    if not _CONTENT_DIR.exists():
        return skills

    # Directory-based skills: content/<skill-name>/SKILL.md
    for skill_dir in sorted(_CONTENT_DIR.iterdir()):
        if skill_dir.is_dir():
            skill_md = skill_dir / "SKILL.md"
            if skill_md.exists():
                content = skill_md.read_text(encoding="utf-8")
                name, description = _parse_frontmatter(skill_dir.name, content)
                skills.append(
                    SkillDefinition(
                        name=name,
                        description=description,
                        content=content,
                        source="bundled",
                        path=str(skill_dir),
                    )
                )

    # Legacy flat .md files: content/*.md
    for path in sorted(_CONTENT_DIR.glob("*.md")):
        content = path.read_text(encoding="utf-8")
        name, description = _parse_frontmatter(path.stem, content)
        skills.append(
            SkillDefinition(
                name=name,
                description=description,
                content=content,
                source="bundled",
                path=str(path),
            )
        )

    return skills


def _parse_frontmatter(default_name: str, content: str) -> tuple[str, str]:
    """Extract name and description from a skill markdown file with YAML frontmatter."""
    name = default_name
    description = ""

    lines = content.splitlines()

    # Try YAML frontmatter (--- ... ---)
    if lines and lines[0].strip() == "---":
        for i, line in enumerate(lines[1:], 1):
            if line.strip() == "---":
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

    # Fallback: heading + first paragraph
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

    return name, description or f"Bundled skill: {name}"
