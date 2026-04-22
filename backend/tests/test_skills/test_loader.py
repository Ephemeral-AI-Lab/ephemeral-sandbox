"""Tests for skill loading."""

from __future__ import annotations

from pathlib import Path

from skills import get_user_skills_dir, load_skill_registry

_BUNDLED_SKILLS_DIR = (
    Path(__file__).resolve().parents[2] / "src" / "skills" / "bundled" / "content"
)


def test_load_skill_registry_includes_bundled(tmp_path: Path, monkeypatch):
    monkeypatch.setenv("EPHEMERALOS_CONFIG_DIR", str(tmp_path / "config"))
    registry = load_skill_registry()

    names = [skill.name for skill in registry.list_skills()]
    assert "team-planner-playbook" in names
    assert "team-replanner-playbook" in names
    assert "team-developer-playbook" in names


def test_load_skill_registry_includes_user_skills(tmp_path: Path, monkeypatch):
    monkeypatch.setenv("EPHEMERALOS_CONFIG_DIR", str(tmp_path / "config"))
    skills_dir = get_user_skills_dir()
    (skills_dir / "deploy.md").write_text("# Deploy\nDeployment workflow guidance\n", encoding="utf-8")

    registry = load_skill_registry()
    deploy = registry.get("Deploy")

    assert deploy is not None
    assert deploy.source == "user"
    assert "Deployment workflow guidance" in deploy.content


def test_team_replanner_playbook_uses_planner_style_contract() -> None:
    skill = (
        _BUNDLED_SKILLS_DIR / "team-replanner-playbook" / "SKILL.md"
    ).read_text(encoding="utf-8")
    contract = (
        _BUNDLED_SKILLS_DIR
        / "team-replanner-playbook"
        / "references"
        / "terminal-contract.md"
    ).read_text(encoding="utf-8")
    action_add = (
        _BUNDLED_SKILLS_DIR
        / "team-replanner-playbook"
        / "references"
        / "action-add-tasks.md"
    ).read_text(encoding="utf-8")
    action_cancel = (
        _BUNDLED_SKILLS_DIR
        / "team-replanner-playbook"
        / "references"
        / "action-cancel-and-redraft.md"
    ).read_text(encoding="utf-8")
    reference_names = {
        path.name
        for path in (_BUNDLED_SKILLS_DIR / "team-replanner-playbook" / "references").glob("*.md")
    }

    assert reference_names == {
        "action-add-tasks.md",
        "action-cancel-and-redraft.md",
        "terminal-contract.md",
    }
    assert len(skill.splitlines()) <= 170
    assert len(action_add.splitlines()) <= 45
    assert len(action_cancel.splitlines()) <= 45
    assert len(contract.splitlines()) <= 160
    assert "## Workflow" in skill
    assert "```mermaid" in skill
    assert "Reference Map" in skill
    assert "terminal-contract" in skill
    assert "Classify Failure Mode" in skill
    assert "Direct replan" in skill
    assert "Diagnostics" in skill
    assert "No production repair surface" in skill
    assert "trace-gap triplets" in skill
    assert "Launch one scout per remaining triplet" in skill
    assert "Keep failing tests in scout `context`, not `target_paths`" in skill

    assert "## Call Shape" in contract
    assert "submit_replan({ new_tasks: NewTaskSpec[], cancel_ids: string[] })" in contract
    assert "## Examples" in contract
    assert "## Final Checklist" in contract
    assert "Final payload shape lives in `terminal-contract`" in action_add
    assert "Final payload shape lives in `terminal-contract`" in action_cancel
    assert "3 or more concrete non-planner replacements" in action_cancel
    assert "terminal validator" in action_cancel
    assert "Example terminal payload" not in action_add
    assert "Example terminal payload" not in action_cancel
    assert "numbered colon labels" not in action_add
    assert "numbered colon labels" not in action_cancel

    assert "2. Task Details:" in skill
    assert "2. Task Details:" in contract
    assert "2. Task Detail:" not in skill
    assert "2. Task Detail:" not in contract
    assert "Valid replan trigger" not in skill
    assert "Replan trigger gate" not in skill


def test_team_planner_playbook_uses_plural_task_details_label() -> None:
    skill = (
        _BUNDLED_SKILLS_DIR / "team-planner-playbook" / "SKILL.md"
    ).read_text(encoding="utf-8")

    assert "2. Task Details:" in skill
    assert "2. Task Detail:" not in skill
    assert "`Task Details`" in skill
    assert "`Task Detail`" not in skill


def test_team_root_planner_playbook_uses_plural_task_details_label() -> None:
    skill = (
        _BUNDLED_SKILLS_DIR / "team-root-planner-playbook" / "SKILL.md"
    ).read_text(encoding="utf-8")

    assert "2. Task Details:" in skill
    assert "2. Task Detail:" not in skill
    assert "`Task Details`" in skill
    assert "`Task Detail`" not in skill


def test_team_validator_playbook_uses_developer_style_contract() -> None:
    validator_dir = _BUNDLED_SKILLS_DIR / "team-validator-playbook"
    skill = (validator_dir / "SKILL.md").read_text(encoding="utf-8")
    reference_files = list((validator_dir / "references").glob("*.md"))

    assert "## Route" in skill
    assert "```mermaid" in skill
    assert "## 1. Read task details" in skill
    assert "## 2. Build validation plan" in skill
    assert "## 3. Run diagnostics and exact verification" in skill
    assert "## 6. Submit terminal summary" in skill
    assert "submit_task_summary({" in skill
    assert 'type: "success" | "request_replan"' in skill
    assert "content: string" in skill
    assert "public-surface guardrail" in skill

    assert reference_files == []
    assert "load_skill_reference" not in skill
    assert "## Conditional references" not in skill
