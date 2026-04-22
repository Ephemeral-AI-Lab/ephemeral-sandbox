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
    assert "Synthesize repair mapping" in skill
    assert "trace-gap triplets" in skill
    assert "Launch one scout per remaining triplet" in skill
    assert "Keep failing tests in scout `context`, not `target_paths`" in skill

    assert "## Call Shape" in contract
    assert "submit_replan({ new_tasks: NewTaskSpec[], cancel_ids: string[] })" in contract
    assert "## Examples" in contract
    assert "## Final Checklist" in contract
    assert "Final payload shape lives in `terminal-contract`" in action_add
    assert "Final payload shape lives in `terminal-contract`" in action_cancel
    assert "separate verification lane" in action_cancel
    assert "local replacement ids it verifies" in action_cancel
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


def test_team_root_planner_playbook_keeps_acceptance_criteria_evidence_focused() -> None:
    skill = (
        _BUNDLED_SKILLS_DIR / "team-root-planner-playbook" / "SKILL.md"
    ).read_text(encoding="utf-8")

    assert "Put benchmark tests and verification commands in `spec`, not `scope_paths`" in skill
    assert "Acceptance Criteria` must be test-suite focused with concrete commands" in skill
    assert "Every `Acceptance Criteria` is test-suite focused" in skill
    assert "CodeAct-safe" not in skill


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


def test_terminal_summary_playbooks_use_shared_replan_taxonomy() -> None:
    allowed = {"scope_expansion", "wrong_owner_or_role", "unresolved_blocker"}
    banned = {
        "dependency_handoff_gap",
        "diagnostic_failure",
        "verification_failure",
        "invalid_command",
        "unmet_acceptance",
        "outside_scope",
        "repair_not_local",
        "investigation_blocker",
        "too_complex_or_out_of_scope",
        "`none`",
    }
    for playbook_name in ("team-developer-playbook", "team-validator-playbook"):
        skill = (_BUNDLED_SKILLS_DIR / playbook_name / "SKILL.md").read_text(
            encoding="utf-8"
        )

        for trigger in allowed:
            assert trigger in skill
        for trigger in banned:
            assert trigger not in skill


def test_developer_playbook_allows_new_file_scope_expansion_only_via_posthook() -> None:
    skill = (
        _BUNDLED_SKILLS_DIR / "team-developer-playbook" / "SKILL.md"
    ).read_text(encoding="utf-8")

    assert "`scope_paths` are the assigned edit surface for existing files" in skill
    assert "You may widen reads, diagnostics, and test commands" in skill
    assert "Acceptance criteria and test outcomes never expand `scope_paths` by themselves" in skill
    assert "A new production file may extend scope only through `daytona_write_file`" in skill
    assert "no other worker owns that exact path" in skill
    assert (
        "The next required change is an existing out-of-scope edit, move, rename, or delete."
        in skill
    )
    assert (
        "new production file whose `daytona_write_file` scope expansion was blocked or conflicted."
        in skill
    )
    assert (
        "Before every mutation, verify the target file path, source path, destination path, or rename file hint"
        in skill
    )
    assert "For a new production file required by live evidence, use `daytona_write_file`" in skill
    assert "If an existing-file mutation is outside scope or the posthook blocks expansion" in skill
    assert "with trigger `scope_expansion`" in skill
    assert (
        "Do not create missing modules, shims, re-exports, or bridges unless live production evidence requires them"
        in skill
    )
    assert (
        "The next required edit is outside `scope_paths`, even when production evidence proves that path is required."
        not in skill
    )


def test_developer_and_validator_playbooks_keep_codeact_api_boundary() -> None:
    for playbook_name in ("team-developer-playbook", "team-validator-playbook"):
        skill = (_BUNDLED_SKILLS_DIR / playbook_name / "SKILL.md").read_text(
            encoding="utf-8"
        )

        assert "use `command` only for Python source snippets" not in skill
        assert "use `code` only for Python source snippets" in skill
        assert "only when no valid equivalent can preserve the needed evidence" in skill
        assert "A pre-hook block after sanitization or another policy denial is terminal tooling evidence" not in skill
        assert "never pass a shell command string in `code`" in skill


def test_validator_playbook_routes_out_of_scope_corrections_to_replan() -> None:
    skill = (
        _BUNDLED_SKILLS_DIR / "team-validator-playbook" / "SKILL.md"
    ).read_text(encoding="utf-8")

    assert "The only apparent correction would edit, move, rename, or delete an existing file" in skill
    assert "Acceptance criteria, dependency handoffs, and test outcomes never expand `scope_paths`" in skill
    assert "by themselves" in skill
    assert "A new production file may extend scope only through `daytona_write_file`" in skill
    assert (
        "new production file whose `daytona_write_file` scope expansion was blocked or conflicted"
        in skill
    )
    assert (
        "Before every mutation, verify the target file is inside an assigned `scope_paths` entry"
        in skill
    )
    assert "For a new production file required by live evidence, use `daytona_write_file`" in skill
    assert "If an existing-file mutation is outside scope or the posthook blocks expansion" in skill
    assert "with trigger `scope_expansion`" in skill
