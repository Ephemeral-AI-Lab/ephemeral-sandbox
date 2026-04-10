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
_SWEEVO_CONTEXT = _BACKEND_ROOT / "src/skills/bundled/content/sweevo-project-context/SKILL.md"


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


def test_planner_playbook_gates_share_briefing_on_tool_availability() -> None:
    planner = _read(_BACKEND_ROOT / "src/skills/bundled/content/team-planner-playbook/SKILL.md")
    assert "only when `share_briefing` is actually available in your tool list" in planner
    assert "calling a tool that is not visibly available" in planner
    assert "representative deduped subset" in planner
    assert "Every entry in `items` must be its own `{...}` object" in planner


def test_sweevo_context_treats_missing_share_briefing_as_non_blocking() -> None:
    sweevo = _read(_SWEEVO_CONTEXT)
    assert "should not spend tool budget on explicit `share_briefing` promotion unless that tool is visibly available" in sweevo
    assert "treat that as a no-promotion profile, not as a blocker" in sweevo
    assert "representative deduped subset of failing ids" in sweevo
    assert "repeat `local_id`, `agent_name`, `kind`, or `payload` keys inside one JSON object" in sweevo


def test_developer_playbook_anchors_import_failures_to_named_pytest_surface() -> None:
    developer = _read(_BACKEND_ROOT / "src/skills/bundled/content/team-developer-playbook/SKILL.md")
    assert "If that first entry point is an import or collection failure" in developer
    assert "Do not promote a probe-only theory into broader code edits" in developer
