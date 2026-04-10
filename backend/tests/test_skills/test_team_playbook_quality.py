"""Quality regressions for team playbook hard-rule sections."""

from __future__ import annotations

import re
from pathlib import Path


_BACKEND_ROOT = Path(__file__).resolve().parents[2]
_PLAYBOOKS = [
    _BACKEND_ROOT / "src/skills/bundled/content/team-developer-playbook/SKILL.md",
    _BACKEND_ROOT / "src/skills/bundled/content/team-validator-playbook/SKILL.md",
    _BACKEND_ROOT / "src/skills/bundled/content/team-posthook-decision-playbook/SKILL.md",
    _BACKEND_ROOT / "src/skills/bundled/content/team-planner-playbook/SKILL.md",
]


def _read(path: Path) -> str:
    return path.read_text(encoding="utf-8")


def _hard_rules_section(content: str) -> str:
    after_header = content.split("## Hard rules", 1)[1]
    return re.split(r"\n---\n|\n## ", after_header, maxsplit=1)[0]


def test_hard_rule_numbers_do_not_repeat() -> None:
    for path in _PLAYBOOKS:
        section = _hard_rules_section(_read(path))
        labels = re.findall(r"^(\d+)\.\s", section, flags=re.MULTILINE)
        assert labels, f"expected numbered hard rules in {path}"
        duplicates = sorted({label for label in labels if labels.count(label) > 1})
        assert not duplicates, f"duplicate hard-rule numbers in {path}: {duplicates}"
