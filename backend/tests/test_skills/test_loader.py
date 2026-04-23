"""Tests for skill loading."""

from __future__ import annotations

from pathlib import Path

from skills import get_user_skills_dir, load_skill_registry

_BUNDLED_SKILLS_DIR = (
    Path(__file__).resolve().parents[2] / "src" / "skills" / "bundled" / "content"
)


def _read_bundled_skill(skill_name: str) -> str:
    return (_BUNDLED_SKILLS_DIR / skill_name / "SKILL.md").read_text(encoding="utf-8")


def _read_bundled_reference(skill_name: str, reference_name: str) -> str:
    return (
        _BUNDLED_SKILLS_DIR / skill_name / "references" / reference_name
    ).read_text(encoding="utf-8")


def _assert_contains_all(content: str, required: tuple[str, ...]) -> None:
    for text in required:
        assert text in content


def _assert_absent(content: str, forbidden: tuple[str, ...]) -> None:
    for text in forbidden:
        assert text not in content


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
    prompt = (
        _BUNDLED_SKILLS_DIR.parents[2]
        / "prompt"
        / "user_prompt"
        / "task_replanner.md"
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
    assert len(skill.splitlines()) <= 260
    assert len(action_add.splitlines()) <= 50
    assert len(action_cancel.splitlines()) <= 45
    assert len(contract.splitlines()) <= 160
    assert "## Workflow Map" in skill
    assert "```mermaid" not in skill
    assert "Decision flow:" in skill
    assert "| Section | Contract |" in skill
    assert "#### Steps" in skill
    assert "Reference Map" in skill
    assert "terminal-contract" in skill
    assert "Every branch must load the matching action reference and then `terminal-contract`" in skill
    assert "Classify Failure Mode" in skill
    assert "Direct replan" in skill
    assert "Diagnostics" in skill
    assert "Synthesize repair mapping" in skill
    assert "check it against every observed value in the same failing assertion" in skill
    assert "compact value table" in skill
    assert "copying the handoff into a repair task" in skill
    assert "trace-gap triplets" in skill
    assert "Launch one scout per remaining triplet" in skill
    assert "Wait for all required `read_task_details` results before calling `read_task_graph()`" in skill
    assert "Do not batch `read_task_graph()` with any required task-detail read" in skill
    assert "Classification: <scope_expansion|wrong_owner_or_role|unresolved_blocker>" in skill
    assert "Diagnostics decision: trivial_direct_replan" in skill
    assert "Diagnostics decision: deep_diagnostics" in skill
    assert "it is never stale sibling work and must stay out of `cancel_ids`" in skill
    assert "If your draft `cancel_ids` contains the failed task id from the prompt" in skill
    assert "If the fix target remains under any failed-task `scope_paths` entry" in skill
    assert "A failed task's \"test design issue\" label does not drop a named fail-to-pass variant" in skill
    assert "Enumerate distinct trace-gap triplets in visible reasoning before any scout call" in skill
    assert '"target_paths": ["<one production path>"]' in skill
    assert "Keep failing tests in scout `context`, not `target_paths`" in skill
    assert "Replanner-created tasks are limited to `developer` repair lanes and `validator` verification lanes" in skill
    assert "Do not submit an empty or no-op replan" in skill

    assert "## Call Shape" in contract
    assert "submit_replan({ new_tasks: NewTaskSpec[], cancel_ids: string[] })" in contract
    assert "Top-level input has only required `new_tasks` and required `cancel_ids`" in contract
    assert "include `cancel_ids: []` when no cancellation is needed" in contract
    assert "Compare every `cancel_ids` entry against the failed task id from the prompt" in contract
    assert "Same-parent graph position does not make the failed task cancellable" in contract
    assert "No `cancel_ids` entry equals the failed task id from the prompt" in contract
    assert "Same-owner-file repairs are not scope expansion" in contract
    assert "`new_tasks` contains at least one corrective task" in contract
    assert "Empty Replan" not in contract
    assert '"new_tasks": []' not in contract
    assert 'name: "developer" | "validator";' in contract
    assert "Every `name` is exactly `developer` or `validator`" in contract
    assert "team_planner is accepted" not in contract
    assert "## Examples" in contract
    assert "## Final Checklist" in contract
    assert (
        "If `Task Details` uses `Classification: unresolved_blocker`, it must also include the exact field `Diagnostics decision: trivial_direct_replan` or `Diagnostics decision: deep_diagnostics`"
        in contract
    )
    assert (
        "Every spec with `Classification: unresolved_blocker` also includes `Diagnostics decision: trivial_direct_replan` or `Diagnostics decision: deep_diagnostics` inside `2. Task Details:`."
        in contract
    )
    assert (
        "Acceptance criteria must not use `-k`, parametrization filters, or prose like \"do not treat this as a repair target\" to avoid a named failing fail-to-pass variant"
        in contract
    )
    assert (
        "No named fail-to-pass variant is dropped as a test design issue, unsupported parametrization, cross-engine mismatch, or \"not a repair target\"."
        in contract
    )
    assert (
        "Do not satisfy this requirement with residual risk prose, \"out of scope\" text"
        in action_add
    )
    assert (
        "No named fail-to-pass variant appears only as residual risk, \"out of scope\", unsupported/test-design prose, broad validator coverage, or validator-only closure without an upstream repair."
        in contract
    )
    assert (
        "A rule that fixes one value while breaking another is not a direct repair"
        in contract
    )
    assert (
        "No proposed one-line rule contradicts another observed value in the same failing assertion."
        in contract
    )
    assert "Account for every named failing variant in the failed task summary." in prompt
    assert "If the failed task proposes a concrete code rule or one-line fix" in prompt
    assert "Final payload shape lives in `terminal-contract`" in action_add
    assert "Final payload shape lives in `terminal-contract`" in action_cancel
    assert "same-scope repair with a named production mechanism is valid" in action_add
    assert "Keep the required top-level `cancel_ids` key explicitly set to `[]`" in action_add
    assert (
        "Dropping a named fail-to-pass variant by labeling it a test design issue, unsupported parametrization, or cross-engine mismatch."
        in action_add
    )
    assert (
        "Keep every named failing variant assigned to a production repair, a diagnostic developer that tests a concrete production seam, or an explicitly identified live repair owner whose task details or terminal summary covers that same variant and seam."
        in action_add
    )
    assert (
        "reject a repair task whose proposed rule cannot satisfy every observed expected/actual row"
        in action_add
    )
    assert "separate verification lane" in action_cancel
    assert "local replacement ids it verifies" in action_cancel
    assert "even when `read_task_graph()` shows it as a same-parent sibling" in action_cancel
    assert "Example terminal payload" not in action_add
    assert "Example terminal payload" not in action_cancel
    assert "numbered colon labels" not in action_add
    assert "numbered colon labels" not in action_cancel
    assert "loaded the action reference matching your cancellation decision" in prompt
    assert "if a validation error still rejects `cancel_ids`" in prompt

    assert "2. Task Details:" in skill
    assert "2. Task Details:" in contract
    assert "2. Task Detail:" not in skill
    assert "2. Task Detail:" not in contract
    assert "Valid replan trigger" not in skill
    assert "Replan trigger gate" not in skill


def test_team_replanner_playbook_defers_references_until_action_and_submit_stages() -> None:
    skill = _read_bundled_skill("team-replanner-playbook")

    assert "Reference boundary: action references belong only to Stage 3" in skill
    assert "`terminal-contract` belongs only to Stage 4" in skill
    assert "A `load_skill_reference(...)` call immediately after `load_skill(...)` is invalid" in skill
    assert "Reference load gates: action reference trigger -> classification is final" in skill
    assert "`terminal-contract` trigger -> the matching action reference has been loaded" in skill
    assert "Failure signal -> `load_skill_reference(...)` appears before context reads" in skill
    assert "before the classification line" in skill
    assert "before diagnostics/scout harvesting finishes" in skill
    assert "loads `terminal-contract` before the matching action reference" in skill
    assert "The action reference load is the stage transition" in skill


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
    reference = (
        _BUNDLED_SKILLS_DIR
        / "team-root-planner-playbook"
        / "references"
        / "synthesize-and-submit.md"
    ).read_text(encoding="utf-8")
    content = f"{skill}\n{reference}"

    assert "2. Task Details:" in content
    assert "2. Task Detail:" not in content
    assert "`Task Details`" in content
    assert "`Task Detail`" not in content


def test_team_root_planner_playbook_keeps_acceptance_criteria_evidence_focused() -> None:
    skill = (
        _BUNDLED_SKILLS_DIR / "team-root-planner-playbook" / "SKILL.md"
    ).read_text(encoding="utf-8")
    reference = (
        _BUNDLED_SKILLS_DIR
        / "team-root-planner-playbook"
        / "references"
        / "synthesize-and-submit.md"
    ).read_text(encoding="utf-8")
    content = f"{skill}\n{reference}"

    assert "Put benchmark tests and verification commands in `spec`, not `scope_paths`" in content
    assert "Acceptance Criteria` must be test-suite focused with concrete commands" in content
    assert "Every `Acceptance Criteria` is test-suite focused" in content
    assert (
        "No fail-to-pass acceptance criterion treats skipped tests, expected failures, clear `ImportError`, or missing optional dependencies as passing closure"
        in content
    )
    assert "Build a coverage ledger for benchmark/fail-to-pass requests" in content
    assert "Do not put a named failing cluster only in a validator spec" in content
    assert (
        "No named fail-to-pass cluster is covered only by a validator without a repair/decomposition owner"
        in content
    )
    assert "CodeAct-safe" not in content


def test_team_root_planner_playbook_requires_parallel_scout_fanout() -> None:
    skill = (
        _BUNDLED_SKILLS_DIR / "team-root-planner-playbook" / "SKILL.md"
    ).read_text(encoding="utf-8")

    assert "Benchmark/fail-to-pass clustering trigger" in skill
    assert "parallel per-family calls before any polling" in skill
    assert "one broad scout bundles unrelated families" in skill
    assert "HDF scout + parquet scout + CLI/config scout" in skill


def test_team_root_planner_playbook_defers_synthesis_reference_until_stage_three() -> None:
    skill = (
        _BUNDLED_SKILLS_DIR / "team-root-planner-playbook" / "SKILL.md"
    ).read_text(encoding="utf-8")

    assert "Stage boundary: `load_skill_reference(...)` belongs only to Stage 3" in skill
    assert "Catalog only. Do not load references from this map during Stage 1 or Stage 2." in skill
    assert "A `load_skill_reference(...)` call immediately after `load_skill(...)` is invalid" in skill
    assert "Reference load gate: trigger -> owner ledger exists" in skill
    assert "load `synthesize-and-submit` as the first Stage 3 action" in skill
    assert 'reference_name="synthesize-and-submit")` appears before Analyze/Scout evidence' in skill
    assert "in the same action that first analyzes failing tests" in skill
    assert "benchmark test paths are verification evidence, not owner proof" in skill
    assert "The reference load is the stage transition" in skill


def test_team_planner_playbook_defers_submit_child_reference_until_stage_three() -> None:
    skill = (
        _BUNDLED_SKILLS_DIR / "team-planner-playbook" / "SKILL.md"
    ).read_text(encoding="utf-8")

    assert "Stage boundary: `load_skill_reference(...)` belongs only to Stage 3" in skill
    assert "Catalog only. Do not load references from this map during Stage 1 or Stage 2." in skill
    assert "Reference load gate: trigger -> own task, parent task, dependency context" in skill
    assert "load `submit-child-plan` as the first Stage 3 action" in skill
    assert 'reference_name="submit-child-plan")` appears before context reads' in skill
    assert "immediately after `load_skill(...)`" in skill
    assert "The reference load is the stage transition" in skill


def test_team_root_planner_playbook_loads_synthesize_submit_reference() -> None:
    skill_dir = _BUNDLED_SKILLS_DIR / "team-root-planner-playbook"
    skill = (skill_dir / "SKILL.md").read_text(encoding="utf-8")
    reference = (skill_dir / "references" / "synthesize-and-submit.md").read_text(
        encoding="utf-8"
    )
    reference_names = {path.name for path in (skill_dir / "references").glob("*.md")}

    assert reference_names == {"synthesize-and-submit.md"}
    assert "## Reference Map" in skill
    assert "`synthesize-and-submit`: clustering and lane selection" in skill
    assert "valid and invalid payload examples" in skill
    assert "task-spec examples for `developer`, `team_planner`, and `validator`" in skill
    assert "dependency DAG examples with rationale" in skill
    assert 'skill_name="team-root-planner-playbook"' in skill
    assert 'reference_name="synthesize-and-submit"' in skill
    assert "Load this reference in Stage 3 before drafting any `submit_plan(...)` payload" in reference
    assert "## Synthesis Rules" in reference
    assert "## Submission Rules" in reference
    assert "## Terminal Tool Contract" in reference
    assert "Root planner policy is stricter than the runtime minimum" in reference
    assert "use only the built-in lane names `developer`, `team_planner`, and `validator`" in reference
    assert "## Payload Examples" in reference
    assert "### Complete Valid Root Payload" in reference
    assert "submit_plan({" in reference
    assert "id: \"val-root-team-runtime\"" in reference
    assert "deps: [\"dev-task-center\", \"plan-team-runtime-cluster\"]" in reference
    assert "### Invalid Payload Shapes" in reference
    assert 'summary: "I made a plan."' in reference
    assert 'parent_id: "root"' in reference
    assert 'scope_paths: ["backend/tests/team/test_task_center.py"]' in reference
    assert 'spec: "1. Goal:\\nRepair the owner.' in reference
    assert "## TaskSpec Examples" in reference
    assert "### Developer TaskSpec" in reference
    assert "### Team Planner TaskSpec" in reference
    assert "### Validator TaskSpec" in reference
    assert "## Dependency DAG Examples" in reference
    assert "### Parallel Fan-Out With Terminal Validator" in reference
    assert "### Sequential Chain" in reference
    assert "### Mixed Sequential And Parallel Work" in reference
    assert "### Planner Output Gating Downstream Integration" in reference
    assert "### Overlapping Scopes Without Scope-Hygiene Deps" in reference
    assert "## Final Checklist" in reference
    assert "## Case Examples" not in reference
    assert "### Case " not in reference


def test_team_root_planner_playbook_prefers_top_down_decomposition() -> None:
    skill = _read_bundled_skill("team-root-planner-playbook")
    reference = _read_bundled_reference(
        "team-root-planner-playbook", "synthesize-and-submit.md"
    )

    _assert_contains_all(
        skill,
        (
            "## Workflow Map",
            "## Reference Map",
            "Decision flow:",
            "### 1. Analyze",
            "### 2. Scout",
            "### 3. Synthesize and submit",
            "load synthesize-and-submit",
            'skill_name="team-root-planner-playbook"',
            'reference_name="synthesize-and-submit"',
            "The root planner routes top-down",
            "child `team_planner`",
            "narrow exact-owner work",
        ),
    )
    _assert_absent(
        skill,
        (
            "## When to Use",
            "## Hierarchical Planning Principle",
            "## Lane Selection",
            "```mermaid",
            "## Terminal Tool Contract",
            "### 4. Submit",
        ),
    )
    _assert_contains_all(
        reference,
        (
            "### Clustering Guidance",
            "### Lane Selection",
            "Clear owner names do not override a clustering signal",
            "large benchmark/test-matrix work",
            "four or more independent `developer` lanes",
            "Use child `team_planner` for broad decomposition",
            "Name-field lock: after classifying a slice, write the `name` field",
            'the only valid `name` is `"team_planner"`',
            "Atomic grouping gate: trigger -> two or more atomic slices have different owner files",
            "one `developer` task spec lists multiple independent fixes across unrelated owners",
            "Multi-API family gate: trigger -> the request or scout notes for one family list multiple public APIs",
            "a `developer` spec calls the family coherent while its `Task Details` lists those APIs or surfaces",
            "Self-consistency gate: trigger -> your synthesis notes call any slice expandable",
            'but the final payload gives that slice `name: "developer"`',
            "## TaskSpec Examples",
            "## Dependency DAG Examples",
        ),
    )
    _assert_absent(reference, ("Depth Gate", "current_depth", "max_depth"))


def test_team_planner_playbook_prefers_recursive_decomposition() -> None:
    skill = _read_bundled_skill("team-planner-playbook")
    reference = _read_bundled_reference(
        "team-planner-playbook", "submit-child-plan.md"
    )

    assert len(skill.splitlines()) <= 130
    _assert_contains_all(
        skill,
        (
            "## Workflow Map",
            "## Reference Map",
            "Decision flow:",
            "### 1. Load context",
            "### 2. Scout",
            "### 3. Synthesize and submit",
            "child `team_planner`",
            "large benchmark/test-matrix work",
            "`grandchild_depth <= max_depth`",
            "broader direct `developer` or `validator` tasks",
            "Name-field lock",
            'never `name: "developer"`',
            "load submit-child-plan",
            'skill_name="team-planner-playbook"',
            'reference_name="submit-child-plan"',
        ),
    )
    _assert_absent(
        skill,
        (
            "## Hierarchical Planning Principle",
            "Team plans are hierarchical",
            "top-down routing",
            "```mermaid",
            "current_depth",
            "Depth rules",
        ),
    )
    _assert_contains_all(
        reference,
        (
            "### Clustering Guidance",
            "### Lane Selection",
            "`grandchild_depth <= max_depth`",
            "use broader direct `developer` and `validator` tasks instead",
            "Name-field lock: after classifying a slice, write the `name` field",
            'must use `name: "team_planner"`, never `name: "developer"`',
            "Atomic grouping gate: trigger -> two or more atomic slices have different owner files",
            "submit separate current-layer `developer` lanes",
            "Multi-API family gate: trigger -> inherited evidence or scout notes for one family list multiple public APIs",
            "classify the family as expandable and use `team_planner` while `grandchild_depth <= max_depth`",
            "Self-consistency gate: trigger -> your synthesis notes call any slice expandable",
            "## TaskSpec Examples",
            "## Dependency DAG Examples",
        ),
    )


def test_planner_playbooks_lock_expandable_slices_out_of_developer_lanes() -> None:
    root_skill = _read_bundled_skill("team-root-planner-playbook")
    root_reference = _read_bundled_reference(
        "team-root-planner-playbook", "synthesize-and-submit.md"
    )
    team_skill = _read_bundled_skill("team-planner-playbook")
    team_reference = _read_bundled_reference(
        "team-planner-playbook", "submit-child-plan.md"
    )

    _assert_contains_all(
        "\n".join((root_skill, root_reference)),
        (
            "Name-field lock",
            "if your synthesis calls a slice expandable",
            'the task\'s `name` must be `team_planner`, never `developer`',
            "Do not create a `developer` task whose own `Goal`, `Task Details`, notes, or checklist rationale calls the same slice expandable",
            '`developer` means the slice passed every atomic test and no expandable signal fired',
            'cannot have `name: "developer"`',
        ),
    )
    _assert_contains_all(
        "\n".join((team_skill, team_reference)),
        (
            "Name-field lock",
            "when `grandchild_depth <= max_depth`",
            'must have `name: "team_planner"`, never `name: "developer"`',
            "do not call the fallback developer lanes atomic",
            '`developer` means the slice passed every atomic test, except for explicit max-depth per-mechanism fallback lanes',
            'cannot have `name: "developer"`',
        ),
    )


def test_team_planner_playbook_requires_fail_to_pass_coverage_owners() -> None:
    content = "\n".join(
        (
            _read_bundled_skill("team-planner-playbook"),
            _read_bundled_reference("team-planner-playbook", "submit-child-plan.md"),
        )
    )

    _assert_contains_all(
        content,
        (
            "coverage ledger",
            "named failing cluster",
            "repair/decomposition",
            "terminal validator",
            "validator spec",
        ),
    )


def test_team_developer_playbook_requires_exact_in_scope_fix_before_replan() -> None:
    skill = (
        _BUNDLED_SKILLS_DIR / "team-developer-playbook" / "SKILL.md"
    ).read_text(encoding="utf-8")

    assert (
        "Do not use `request_replan` as a handoff for exact code you already know how to change"
        in skill
    )
    assert "assigned-scope or adjacent production-path actionable code defect" in skill


def test_planner_and_scout_playbooks_keep_benchmark_tests_as_evidence() -> None:
    planner_skill = (
        _BUNDLED_SKILLS_DIR / "team-planner-playbook" / "SKILL.md"
    ).read_text(encoding="utf-8")
    planner_reference = (
        _BUNDLED_SKILLS_DIR
        / "team-planner-playbook"
        / "references"
        / "submit-child-plan.md"
    ).read_text(encoding="utf-8")
    planner_content = f"{planner_skill}\n{planner_reference}"
    root_planner_skill = (
        _BUNDLED_SKILLS_DIR / "team-root-planner-playbook" / "SKILL.md"
    ).read_text(encoding="utf-8")
    root_planner_reference = (
        _BUNDLED_SKILLS_DIR
        / "team-root-planner-playbook"
        / "references"
        / "synthesize-and-submit.md"
    ).read_text(encoding="utf-8")
    root_planner_content = f"{root_planner_skill}\n{root_planner_reference}"
    scout_skill = (
        _BUNDLED_SKILLS_DIR / "team-scout-playbook" / "SKILL.md"
    ).read_text(encoding="utf-8")
    scout_contract = (
        _BUNDLED_SKILLS_DIR
        / "team-scout-playbook"
        / "references"
        / "completion-contract.md"
    ).read_text(encoding="utf-8")

    for skill in (planner_content, root_planner_content):
        _assert_contains_all(
            skill,
            (
                "target_paths",
                "production-only",
                "scout `context`",
                "optional-dependency errors",
                "benchmark",
                "`spec`",
                "`scope_paths`",
                "skip",
                "xfail",
                "pytest configuration",
                "evidence only",
            ),
        )

    for skill in (scout_skill, scout_contract):
        assert "Never prescribe" in skill or "Must not recommend" in skill
        assert "skipping" in skill
        assert "xfail" in skill
        assert "pytest configuration" in skill


def test_team_validator_playbook_uses_root_planner_style_contract() -> None:
    validator_dir = _BUNDLED_SKILLS_DIR / "team-validator-playbook"
    skill = (validator_dir / "SKILL.md").read_text(encoding="utf-8")
    reference_files = list((validator_dir / "references").glob("*.md"))

    assert "## Workflow Map" in skill
    assert "Decision flow:" in skill
    assert "## Workflow Details" in skill
    assert "| Section | Contract |" in skill
    assert "#### Steps" in skill
    assert "```mermaid" not in skill
    assert "### 1. Read task details" in skill
    assert "### 2. Build validation plan" in skill
    assert "### 3. Run diagnostics and exact verification" in skill
    assert "### 6. Submit terminal summary" in skill
    assert "submit_task_summary({" in skill
    assert 'type: "success" | "request_replan"' in skill
    assert "content: string" in skill
    assert "public-surface guardrail" in skill

    assert reference_files == []
    assert "load_skill_reference" not in skill
    assert "## Conditional references" not in skill


def test_team_developer_playbook_uses_root_planner_style_contract() -> None:
    skill = (
        _BUNDLED_SKILLS_DIR / "team-developer-playbook" / "SKILL.md"
    ).read_text(encoding="utf-8")

    assert "## Workflow Map" in skill
    assert "Decision flow:" in skill
    assert "## Workflow Details" in skill
    assert "| Section | Contract |" in skill
    assert "#### Steps" in skill
    assert "```mermaid" not in skill
    assert "### 1. Read task details" in skill
    assert "### 2. Plan" in skill
    assert "### 3. Implement" in skill
    assert "### 4. Verify" in skill
    assert "### 5. Root cause analysis" in skill
    assert "### 6. Submit terminal summary" in skill
    assert "submit_task_summary({" in skill
    assert 'type: "success" | "request_replan"' in skill
    assert "content: string" in skill


def test_developer_and_validator_playbooks_do_not_include_depth_gate_policy() -> None:
    for playbook_name in ("team-developer-playbook", "team-validator-playbook"):
        skill = (_BUNDLED_SKILLS_DIR / playbook_name / "SKILL.md").read_text(
            encoding="utf-8"
        )

        assert "Depth Gate" not in skill
        assert "Planning depth" not in skill
        assert "current_depth" not in skill
        assert "max_depth" not in skill
        assert "child_depth" not in skill
        assert "grandchild_depth" not in skill


def test_terminal_summary_playbooks_require_explicit_residual_risk() -> None:
    for playbook_name in ("team-developer-playbook", "team-validator-playbook"):
        skill = (_BUNDLED_SKILLS_DIR / playbook_name / "SKILL.md").read_text(
            encoding="utf-8"
        )

        assert "Do not omit a line because the answer is \"none\"" in skill
        assert '`Residual Risk:` with remaining risk, unverified surface, or "none"' in skill


def test_developer_playbook_rejects_success_without_runtime_verification() -> None:
    skill = (
        _BUNDLED_SKILLS_DIR / "team-developer-playbook" / "SKILL.md"
    ).read_text(encoding="utf-8")

    assert "Clean diagnostics are not acceptance verification" in skill
    assert "the required runtime command was not run after the final edit" in skill
    assert "verification was not run, was skipped due to budget" in skill
    assert "pass or skip" in skill
    assert "ended in collection/import/no-tests/optional-dependency failure" in skill
    assert "supported only by diagnostics is not a success summary" in skill


def test_developer_playbook_rejects_wrapped_or_suppressed_verification() -> None:
    skill = (
        _BUNDLED_SKILLS_DIR / "team-developer-playbook" / "SKILL.md"
    ).read_text(encoding="utf-8")

    assert "Do not wrap required pytest/build verification" in skill
    assert "`subprocess.run`" in skill
    assert "`PYTHONWARNINGS`" in skill
    assert "`--override-ini`" in skill
    assert "`filterwarnings=`" in skill
    assert "pytest-config-overridden" in skill
    assert "manual `print(\"EXIT CODE\")`" in skill
    assert "tool-reported command exit code" in skill
    assert "A wrapper that prints an inner exit code" in skill
    assert "keep that raw failure as evidence" in skill
    assert "was wrapped, warning-suppressed, pytest-config-overridden" in skill


def test_validator_playbook_rejects_pytest_config_override_evidence() -> None:
    skill = (
        _BUNDLED_SKILLS_DIR / "team-validator-playbook" / "SKILL.md"
    ).read_text(encoding="utf-8")

    assert "Do not suppress or alter pytest configuration" in skill
    assert "`--override-ini`" in skill
    assert "`filterwarnings=`" in skill
    assert "pytest-config-overridden command" in skill


def test_developer_playbook_keeps_parametrized_f2p_variants_as_evidence() -> None:
    skill = (
        _BUNDLED_SKILLS_DIR / "team-developer-playbook" / "SKILL.md"
    ).read_text(encoding="utf-8")

    assert (
        "Do not dismiss a fail-to-pass parametrized variant as a test design issue"
        in skill
    )
    assert "Keep it as production compatibility evidence" in skill
    assert (
        "Missing optional dependencies, older dependency versions, and unavailable engines are not final blockers"
        in skill
    )
    assert "production guard, fallback, explicit compatibility error" in skill
    assert "do not request replan just because the sandbox lacks the package or version" in skill
    assert (
        "it must not list install, dependency upgrade, skip, xfail, pytest config, test edit, or environment replacement as the path forward"
        in skill
    )


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


def test_developer_playbook_allows_advisory_out_of_scope_production_edits() -> None:
    skill = (
        _BUNDLED_SKILLS_DIR / "team-developer-playbook" / "SKILL.md"
    ).read_text(encoding="utf-8")

    assert "`scope_paths` are the primary ownership surface, not a hard mutation sandbox" in skill
    assert "You may widen reads, diagnostics, and test commands" in skill
    assert "Developers may write, copy, or create production files outside `scope_paths`" in skill
    assert "outside-scope system notification" in skill
    assert "coordination guidance, not a stop condition" in skill
    assert (
        "The next required change would be a broad or ambiguous production change that this lane cannot responsibly finish."
        in skill
    )
    assert (
        "ambiguous new production file whose missing path and mechanism are not proven by live production evidence."
        in skill
    )
    assert (
        "Before every mutation, verify the target file path, source path, destination path, or rename file hint"
        in skill
    )
    assert "Out-of-scope production writes, copies, and new files are allowed for developers" in skill
    assert "write-scope notifications are recorded" in skill
    assert "treat it as coordination context and keep working" in skill
    assert "with trigger `scope_expansion`" in skill
    assert (
        "Do not create missing modules, shims, re-exports, or bridges unless live production evidence names the missing path and mechanism"
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
        assert "commands already start at the sandbox repo root" in skill
        assert "never `cd` to a host/local workspace path" in skill
        assert "Never prefix commands with `cd /testbed &&`" in skill


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
    assert "an advisory warning is workflow evidence, not permission to continue editing" in skill
    assert "with trigger `scope_expansion`" in skill
