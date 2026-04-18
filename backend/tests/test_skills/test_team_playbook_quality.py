"""Quality checks for bundled team playbooks."""

from __future__ import annotations

import re
from pathlib import Path


_BACKEND_ROOT = Path(__file__).resolve().parents[2]
_CONTENT = _BACKEND_ROOT / "src/skills/bundled/content"
_PLAYBOOKS = [
    _CONTENT / "team-developer-playbook/SKILL.md",
    _CONTENT / "team-validator-playbook/SKILL.md",
    _CONTENT / "team-planner-playbook/SKILL.md",
    _CONTENT / "team-replanner-playbook/SKILL.md",
    _CONTENT / "team-scout-playbook/SKILL.md",
]
_PLAYBOOKS = [path for path in _PLAYBOOKS if path.exists()]
_ALL_SKILLS = _PLAYBOOKS + [
    _CONTENT / "sweevo-project-context/SKILL.md",
    _CONTENT / "verification-replan/SKILL.md",
]
_ALL_SKILLS = [path for path in _ALL_SKILLS if path.exists()]
_REFERENCES = [
    _CONTENT / "team-developer-playbook/references/codeact-runtime-examples.md",
    _CONTENT / "team-developer-playbook/references/pre-completion-validation.md",
    _CONTENT / "team-developer-playbook/references/root-cause-debugging.md",
    _CONTENT / "team-developer-playbook/references/widening-and-runtime.md",
    _CONTENT / "team-planner-playbook/references/exploration-script.md",
    _CONTENT / "team-planner-playbook/references/scout-launch-contract.md",
    _CONTENT / "team-planner-playbook/references/non-root-context-reuse.md",
    _CONTENT / "team-planner-playbook/references/task-planning-decomposition.md",
    _CONTENT / "team-planner-playbook/references/root-plan-self-check.md",
    _CONTENT / "team-planner-playbook/references/plan-json-contract.md",
    _CONTENT / "team-planner-playbook/references/terminal-validation-contract.md",
    _CONTENT / "team-scout-playbook/references/completion-contract.md",
    _CONTENT / "team-validator-playbook/references/cross-surface-guardrails.md",
    _CONTENT / "team-validator-playbook/references/runtime-verification-examples.md",
    _CONTENT / "team-replanner-playbook/references/corrective-fast-path.md",
    _CONTENT / "team-replanner-playbook/references/action-add-tasks.md",
    _CONTENT / "team-replanner-playbook/references/action-cancel-and-redraft.md",
    _CONTENT / "verification-replan/references/triage-format.md",
]
_REFERENCES = [path for path in _REFERENCES if path.exists()]


def _read(path: Path) -> str:
    return path.read_text(encoding="utf-8")


def _hard_rules_section(content: str) -> str:
    after_header = content.split("## Hard rules", 1)[1]
    return re.split(r"\n## ", after_header, maxsplit=1)[0]


def test_skills_and_references_stay_short() -> None:
    for path in _ALL_SKILLS:
        assert len(_read(path).splitlines()) <= 150, f"{path} should stay short"
    for path in _REFERENCES:
        limit = 180 if path.name == "task-planning-decomposition.md" else 150
        assert len(_read(path).splitlines()) <= limit, f"{path} should stay short"


def test_hard_rule_numbers_do_not_repeat() -> None:
    for path in _PLAYBOOKS:
        section = _hard_rules_section(_read(path))
        labels = re.findall(r"^(\d+)\.\s", section, flags=re.MULTILINE)
        assert labels, f"expected numbered hard rules in {path}"
        assert labels == [str(i) for i in range(1, len(labels) + 1)], f"bad numbering in {path}"


def test_skills_use_clear_must_never_language() -> None:
    for path in _ALL_SKILLS:
        content = _read(path)
        assert (
            "Must " in content
            or "Must\n" in content
            or "Must use" in content
            or "Must treat" in content
        )
        assert (
            "Never " in content
            or "Never\n" in content
            or "Never use" in content
            or "Do not " in content
            or "do not " in content
            or "Must not " in content
        )

    for path in _REFERENCES:
        content = _read(path)
        assert (
            "Use this reference" in content
            or "Use this reference only" in content
            or content.startswith("# Action Reference:")
        )


def test_team_references_follow_scan_friendly_structure() -> None:
    team_references = [
        path for path in _REFERENCES if "/team-" in str(path)
    ]
    for path in team_references:
        content = _read(path)
        assert "## Task/Goal" in content, f"missing Task/Goal section in {path}"
        assert "## Avoid" in content, f"missing Avoid section in {path}"
        assert "## Workflow" in content, f"missing Workflow section in {path}"
        assert "## Expected Outcome" in content, f"missing Expected Outcome section in {path}"


def test_team_playbooks_load_references_for_detail_and_keep_top_level_generic() -> None:
    planner = _read(_CONTENT / "team-planner-playbook/SKILL.md")
    developer = _read(_CONTENT / "team-developer-playbook/SKILL.md")
    validator = _read(_CONTENT / "team-validator-playbook/SKILL.md")
    replanner = _read(_CONTENT / "team-replanner-playbook/SKILL.md")
    scout = _read(_CONTENT / "team-scout-playbook/SKILL.md")

    assert "must load `exploration-script`" in planner.lower()
    assert "must load `scout-launch-contract`" in planner.lower()
    assert "must load `task-planning-decomposition`" in planner.lower()
    assert "final submit helper" in planner.lower()
    assert "submit_plan` tool schema is enough" in planner
    assert "top-level `deps` field lists every same-layer non-validator sibling id" in planner
    assert "child `team_planner` decomposition lanes" in planner
    assert "prose inside `spec` does not create task dependencies" in planner
    assert "run_subagent scout notes are current-task notes" in planner
    assert 'do not use `scope="sibling"` for them' in planner
    assert "scrub each scout `target_paths` list before calling `run_subagent`" in planner
    assert "live production owner files/directories only" in planner
    assert "never submit a `validator` task with `deps: []`" in planner.lower()
    assert "never omit same-layer `team_planner` siblings from validator `deps`" in planner.lower()
    assert "do not put those paths in `scope_paths` for developer or child-planner lanes" in planner
    assert "scope_paths` to production owner paths" in planner
    assert "never put verification-only benchmark tests in developer or child-planner `scope_paths`" in planner.lower()
    assert "never pass `*/tests/*`, `test_*.py`, or unconfirmed test-derived paths in scout `target_paths`" in planner.lower()
    assert "never guess an exact owner" in planner.lower()
    assert "never make non-submission tool calls after loading `plan-json-contract`" in planner.lower()
    assert "split unrelated scout targets" in planner.lower()
    assert "compat/re-export" not in planner
    assert "utils_dataframe.py" not in planner

    assert "must load `root-cause-debugging`" in developer.lower()
    assert "must load `widening-and-runtime`" in developer.lower()
    assert "must load `codeact-runtime-examples`" in developer.lower()
    assert "must load `pre-completion-validation`" in developer.lower()
    assert "before any `daytona_read_file(...)`" in developer
    assert "Empty note reads are successful freshness checks." in developer
    assert "never rewrite benchmark tests" in developer.lower()
    assert "must not use `daytona_codeact` for file edits" in developer.lower()
    assert "must not use `daytona_codeact` for file-content reads" in developer.lower()
    assert "writes to test files as off-policy" in developer.lower()
    assert "test files in `scope_paths` as read/verify-only" in developer.lower()
    assert "never treat test paths in `scope_paths` as edit permission" in developer.lower()
    assert "uid 0 bypassing" not in developer.lower()
    assert "pkg._compatibility" not in developer

    assert "must load `cross-surface-guardrails`" in validator.lower()
    assert "must load `runtime-verification-examples`" in validator.lower()
    assert "before any `daytona_read_file(...)`" in validator
    assert "must not paraphrase failure evidence" in validator.lower()
    assert "small local corrective patch" in validator.lower()
    assert "must not use `daytona_codeact` for corrective edits" in validator.lower()
    assert "must not use `daytona_codeact` for file-content reads" in validator.lower()
    assert "writes to test files as off-policy" in validator.lower()
    assert 'submit_task_summary(type="fail", content=...)' in validator
    assert "repeated repair attempts" in validator.lower()

    assert "must load `corrective-fast-path`" in replanner.lower()
    assert "must load `action-add-tasks`" in replanner.lower()
    assert "must load `action-cancel-and-redraft`" in replanner.lower()
    assert 'read_task_note(paths=[...], scope="sibling")' in replanner
    assert "final-action ordering" in replanner.lower()

    assert "must load `completion-contract`" in scout.lower()
    assert "must not edit files" in scout.lower()
    assert "must keep missing targets missing" in scout.lower()
    assert "unconfirmed adjacent evidence" in scout.lower()
    assert "must call exactly one `submit_task_note(...)`" in scout.lower()
    assert "never use final prose instead of `submit_task_note(...)`" in scout.lower()
    assert "must not end with only visible findings" in scout.lower()


def test_reference_files_hold_specialized_detail() -> None:
    planner_ref = _read(_CONTENT / "team-planner-playbook/references/exploration-script.md")
    planner_json = _read(_CONTENT / "team-planner-playbook/references/plan-json-contract.md")
    planner_decomposition = _read(
        _CONTENT / "team-planner-playbook/references/task-planning-decomposition.md"
    )
    developer_runtime = _read(
        _CONTENT / "team-developer-playbook/references/codeact-runtime-examples.md"
    )
    developer_playbook = _read(_CONTENT / "team-developer-playbook/SKILL.md")
    developer_root_cause = _read(
        _CONTENT / "team-developer-playbook/references/root-cause-debugging.md"
    )
    scout_ref = _read(_CONTENT / "team-scout-playbook/references/completion-contract.md")
    validator_ref = _read(
        _CONTENT / "team-validator-playbook/references/runtime-verification-examples.md"
    )

    assert "Never keep a guessed exact leaf once live evidence disproves it." in planner_ref
    assert "read_task_note(paths=[...])` with default scope" in planner_ref
    assert "optional final helper" in planner_json
    assert "submit_plan(new_tasks=[...])" in planner_json
    assert "Do not include `task_note`" in planner_json
    assert "`1. Goal:`" in planner_json
    assert "Do not use Markdown headings" in planner_json
    assert "Mentioning dependencies inside `spec` does not set task deps" in planner_json
    assert "verification-only test targets in `spec` context or acceptance criteria" in planner_json
    assert "child planners like `plan-parquet` or `plan-groupby`" in planner_json
    assert "Never submit it with `deps: []`" in planner_json
    assert "Example task graph" in planner_decomposition
    assert '"id": "dev-hdf"' in planner_decomposition
    assert '"id": "dev-shared-config"' in planner_decomposition
    assert '"id": "plan-dataframe-io"' in planner_decomposition
    assert '"id": "plan-groupby"' in planner_decomposition
    assert "Use `developer` for atomic tasks" in planner_decomposition
    assert "Use `team_planner` for expandable tasks" in planner_decomposition
    assert "Use `validator` for validation tasks" in planner_decomposition
    assert "first-wave scout has been launched and its notes reviewed" in planner_decomposition
    scout_launch = _read(
        _CONTENT / "team-planner-playbook/references/scout-launch-contract.md"
    )
    assert "Notes from `run_subagent` scouts live on the current planner task" in scout_launch
    assert 'do not use `scope="sibling"` for them' in scout_launch
    assert "Scrub `target_paths` first" in scout_launch
    assert "missing test-derived path in scout `target_paths`" in scout_launch
    assert 'daytona_codeact(command="...", timeout=N)' in developer_runtime
    assert "Must not append shell capture plumbing" in developer_runtime
    assert "Must not edit files through CodeAct" in developer_runtime
    assert "Must not inspect source through CodeAct" in developer_runtime
    assert "cd /testbed" in developer_playbook
    assert "cd /testbed" in developer_runtime
    assert "pkg._compat" in developer_root_cause
    assert "The Task Center note is the durable handoff." in scout_ref
    assert "Make exactly one `submit_task_note(...)` call" in scout_ref
    assert "assistant text with no `submit_task_note(...)` call" in scout_ref
    assert "check_background_progress" in validator_ref
    assert "Must not inspect source through CodeAct" in validator_ref


def test_replanner_references_spell_valid_submit_replan_payload_shape() -> None:
    replanner = _read(_CONTENT / "team-replanner-playbook/SKILL.md")
    add_tasks = _read(_CONTENT / "team-replanner-playbook/references/action-add-tasks.md")
    cancel_redraft = _read(
        _CONTENT / "team-replanner-playbook/references/action-cancel-and-redraft.md"
    )
    corrective_fast_path = _read(
        _CONTENT / "team-replanner-playbook/references/corrective-fast-path.md"
    )

    assert "pairwise-check `new_tasks`" in replanner
    assert "Parallel concrete tasks must not share any `scope_paths` file" in add_tasks
    assert "parallel tasks that share an owner file" in corrective_fast_path

    for content in (add_tasks, cancel_redraft):
        assert "`1. Goal:`" in content
        assert "`2. Environment:`" in content
        assert "`3. Scope:`" in content
        assert "`4. Context:`" in content
        assert "`5. Acceptance Criteria:`" in content
        assert "Do not use Markdown headings" in content
        assert "Do not include `task_note`" in content


def test_sweevo_context_stays_shared_and_runtime_focused() -> None:
    sweevo = _read(_CONTENT / "sweevo-project-context/SKILL.md")
    assert "Must report a missing named test or node as `benchmark_surface_mismatch`." in sweevo
    assert (
        "Must not label a missing transitive import, helper, or adjacent production module as `benchmark_surface_mismatch`"
        in sweevo
    )
    assert "Must keep commands repo-root-relative." in sweevo
    assert 'daytona_codeact(command="...", timeout=N)' in sweevo
    assert "Must treat `daytona_codeact` as runtime-only" in sweevo
    assert "Python process wrappers" in sweevo
    assert "cd /testbed" in sweevo
    assert "stdout/stderr capture plumbing" in sweevo
    assert "`2>/dev/null`" in sweevo
    assert "Must keep roles separate" in sweevo
    assert "Must treat `docs/architecture/team-coordination.md` as the design intent" in sweevo
    assert "Must keep shared context in the Task Center" in sweevo
    assert "Must prefer Task Center notes, exact runtime evidence, and CI symbol tools over raw file reads on ready owner lanes." in sweevo
    assert "Must not spend a ready leaf's opening moves reading benchmark tests" in sweevo
    assert (
        "must not create planner/scout ownership tasks whose scope is benchmark-test archaeology"
        in sweevo.lower()
    )
    assert "Must not derive an exact production file from benchmark filename resemblance alone" in sweevo
    assert "Must use `read_task_note(paths=[...])` before opening source files" in sweevo
    assert "Must not use `daytona_codeact` for source inspection" in sweevo
    assert "Must treat scope-change notifications and `task_center_changed_since()` as freshness signals." in sweevo
    assert "workflow rules are prompt/playbook obligations" in sweevo
    assert "Must keep `scope_paths` as soft coordination hints" in sweevo
    assert "Must treat test-file writes as off-policy" in sweevo
    assert "Must treat any advisory outside-scope write as a tainted packet" in sweevo
    assert "Use `daytona_read_file(...)` only after notes plus CI identify a narrow line range" in sweevo


def test_worker_playbooks_do_not_mention_submitters_or_action_routing() -> None:
    for path in (
        _CONTENT / "team-developer-playbook/SKILL.md",
        _CONTENT / "team-validator-playbook/SKILL.md",
        _CONTENT / "sweevo-project-context/SKILL.md",
    ):
        content = _read(path)
        assert "submit_summary" not in content
        assert "submit_replan" not in content
        assert "request_retry" not in content
        assert "RECOMMENDED_ACTION" not in content
