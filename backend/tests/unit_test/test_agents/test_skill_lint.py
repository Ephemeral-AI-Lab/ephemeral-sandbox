"""Round 3 Phase 5: startup lint over AgentDefinition.skill paths."""

from __future__ import annotations

from pathlib import Path

import pytest

from agents import AgentDefinition, AgentKind
from agents.skills import (
    SkillLintError,
    scan_skill_file,
    validate_skill_files,
)


def _write_skill(tmp_path: Path, body: str) -> Path:
    path = tmp_path / "SKILL.md"
    path.write_text(f"---\nname: x\n---\n\n{body}")
    return path


def test_scan_passes_on_terminal_silent_skill(tmp_path: Path):
    path = _write_skill(
        tmp_path,
        "Drive to the decision point; the submission step is downstream.",
    )
    assert scan_skill_file(path) == []


def test_scan_rejects_submit_substring(tmp_path: Path):
    path = _write_skill(tmp_path, "Pick submit_plan_closes_goal at the end.")
    violations = scan_skill_file(path)
    assert violations
    assert any("submit_plan_closes_goal" in v for v in violations)


def test_scan_rejects_invented_submit_substring(tmp_path: Path):
    """Lint matches any submit_* pattern, not just registered keys."""
    path = _write_skill(tmp_path, "Call submit_handcrafted_terminal here.")
    violations = scan_skill_file(path)
    assert any("submit_handcrafted_terminal" in v for v in violations)


def test_scan_passes_on_bridging_language(tmp_path: Path):
    path = _write_skill(
        tmp_path,
        "Verify the deliverable, then reach the decision point.",
    )
    assert scan_skill_file(path) == []


def test_scan_ignores_frontmatter(tmp_path: Path):
    """Frontmatter is stripped before scanning — author metadata is safe."""
    path = tmp_path / "SKILL.md"
    path.write_text(
        "---\nname: submit_plan_closes_goal_doc\n---\n\nClean body."
    )
    assert scan_skill_file(path) == []


def test_validate_skill_files_raises_with_definition_name(tmp_path: Path):
    path = _write_skill(tmp_path, "Submit submit_plan_continues_goal now.")
    bad = AgentDefinition(
        name="bad_planner",
        description="bad",
        agent_kind=AgentKind.PLANNER,
        skill=path,
    )

    with pytest.raises(SkillLintError) as exc:
        validate_skill_files([bad])
    assert "bad_planner" in str(exc.value)
    assert "submit_plan_continues_goal" in str(exc.value)


def test_validate_skill_files_ignores_definitions_without_skill():
    """No skill declared → no lint and no raise."""
    plain = AgentDefinition(
        name="plain",
        description="plain",
        agent_kind=AgentKind.EXECUTOR,
    )
    validate_skill_files([plain])  # must not raise


def test_validate_skill_files_aggregates_violations(tmp_path: Path):
    bad1 = tmp_path / "a" / "SKILL.md"
    bad1.parent.mkdir()
    bad1.write_text("---\nname: x\n---\n\nsubmit_first_terminal")
    bad2 = tmp_path / "b" / "SKILL.md"
    bad2.parent.mkdir()
    bad2.write_text("---\nname: x\n---\n\nsubmit_second_terminal")

    defs = [
        AgentDefinition(
            name="planner_a",
            description="a",
            agent_kind=AgentKind.PLANNER,
            skill=bad1,
        ),
        AgentDefinition(
            name="planner_b",
            description="b",
            agent_kind=AgentKind.PLANNER,
            skill=bad2,
        ),
    ]
    with pytest.raises(SkillLintError) as exc:
        validate_skill_files(defs)
    msg = str(exc.value)
    assert "submit_first_terminal" in msg
    assert "submit_second_terminal" in msg
    assert "planner_a" in msg
    assert "planner_b" in msg


def test_shipped_planner_skills_pass_lint():
    """The two skill files that ship in backend/config/skills/ must be clean."""
    from agents import load_agents_tree

    profiles = Path(__file__).resolve().parents[3] / "src" / "agents" / "profile"
    defs = [
        d
        for d in load_agents_tree(profiles)
        if d.skill is not None
    ]
    # Sanity: both planner variants exist and declare skills.
    names = {d.name for d in defs}
    assert {"planner", "planner_full_only"}.issubset(names)
    # Must not raise.
    validate_skill_files(defs)
