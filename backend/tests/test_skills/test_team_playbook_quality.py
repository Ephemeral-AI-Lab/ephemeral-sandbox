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
    assert "never launch `run_subagent` scouts on benchmark test paths" in planner.lower()
    assert "use scouts to locate or correct benchmark test paths" in planner.lower()
    assert "scout the production owner path instead" in planner
    assert "never submit a `validator` task with `deps: []`" in planner.lower()
    assert "never omit same-layer `team_planner` siblings from validator `deps`" in planner.lower()
    assert "must pairwise-check concrete non-planner tasks before `submit_plan(...)`" in planner.lower()
    assert "never use a failed `submit_plan(...)` result to learn that parallel concrete tasks overlap" in planner.lower()
    assert "do not put those paths in `scope_paths` for developer, validator, or child-planner lanes" in planner
    assert "scope_paths` to production owner paths" in planner
    assert "never put verification-only benchmark tests in developer, validator, or child-planner `scope_paths`" in planner.lower()
    assert "never pass `*/tests/*`, `test_*.py`, or unconfirmed test-derived paths in scout `target_paths`" in planner.lower()
    assert "locate/correct benchmark test paths" in planner
    assert "never guess an exact owner" in planner.lower()
    assert "never make non-submission tool calls after loading `plan-json-contract`" in planner.lower()
    assert "missing modules, compatibility shims, re-export modules, and import bridges named only by tests" in planner
    assert "new-file owner needs non-test production evidence" in planner
    assert "no indexed symbols for that file" in planner
    assert "Do not keep the exact file in scout `target_paths` or any `scope_paths`" in planner
    assert "Never carry a disproved exact file into `scope_paths`" in planner
    assert "read the posted Task Center notes instead of checking or waiting on that id again" in planner
    assert "Never call `check_background_progress(...)` or `wait_for_background_task(...)` again" in planner
    assert "standard re-export pattern" in planner
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
    assert "must not use `daytona_codeact` for file writes or moves" in developer.lower()
    assert "pure removals such as `rm`, `unlink`, `os.remove`" in developer.lower()
    assert "must not use `daytona_codeact` for file-content reads" in developer.lower()
    assert "writes to test files as off-policy" in developer.lower()
    assert "test files in `scope_paths` as read/verify-only" in developer.lower()
    assert "must not create, rename, move, or re-export a path outside `scope_paths`" in developer.lower()
    assert "must not create a new file from test-import evidence alone" in developer.lower()
    assert "scope_paths` names an absent module" in developer
    assert "compatibility shim, re-export module, or import bridge" in developer
    assert "missing module named by tests or collection is a stop signal" in developer
    assert "ModuleNotFoundError" in developer
    assert "daytona_glob" in developer
    assert "daytona_grep" in developer
    assert "ci_query_symbol" in developer
    assert "multiple tests import it" in developer
    assert "similar in-scope compatibility file" in developer
    assert "check both source and destination" in developer.lower()
    assert "in-scope source path is not permission" in developer.lower()
    assert "Never keep working after an outside-scope missing-module import or collection failure" in developer
    assert "Never treat a similar in-scope compatibility module" in developer
    assert "must not retry the same delete/move tool" in developer.lower()
    assert "git-history inspection" in developer
    assert "Never retry a failed `daytona_delete_file` or `daytona_move_file` call" in developer
    assert "Never use git history, test-source archaeology, or another search" in developer
    assert "needed to make tests collect" in developer.lower()
    assert "similar in-scope compatibility file" in developer.lower()
    assert "attempt itself is a failed lane" in developer.lower()
    assert "do not read, inspect, edit, run tests, or verify after the warning" in developer.lower()
    assert "submit_task_summary(type=\"request_replan\", content=...)" in developer
    assert "never treat test paths in `scope_paths` as edit permission" in developer.lower()
    assert "never create an outside-scope compatibility shim" in developer.lower()
    assert "uid 0 bypassing" not in developer.lower()
    assert "pkg._compatibility" not in developer

    assert "must load `cross-surface-guardrails`" in validator.lower()
    assert "must load `runtime-verification-examples`" in validator.lower()
    assert "before any `daytona_read_file(...)`" in validator
    assert "must not paraphrase failure evidence" in validator.lower()
    assert "small local corrective patch" in validator.lower()
    assert "must not use `daytona_codeact` for corrective writes or moves" in validator.lower()
    assert "pure removals such as `rm`, `unlink`, `os.remove`" in validator.lower()
    assert "must not use `daytona_codeact` for file-content reads" in validator.lower()
    assert "writes to test files as off-policy" in validator.lower()
    assert 'submit_task_summary(type="request_replan", content=...)' in validator
    assert "repeated repair attempts" in validator.lower()

    assert "must load `corrective-fast-path`" in replanner.lower()
    assert "must load `action-add-tasks`" in replanner.lower()
    assert "must load `action-cancel-and-redraft`" in replanner.lower()
    assert 'read_task_note(paths=[...], scope="sibling")' in replanner
    assert "final-action ordering" in replanner.lower()
    assert "missing modules, compatibility shims, re-export modules, import bridges, file renames, and file moves named only by tests" in replanner
    assert "non-test production evidence proves the absent path" in replanner
    assert "similar in-scope compatibility filename is not an exception" in replanner
    assert "check both source and destination" in replanner.lower()
    assert "in-scope source compatibility file is not permission" in replanner.lower()
    assert "Never treat a similar in-scope compatibility module" in replanner
    assert "tests out of corrective `scope_paths`" in replanner
    assert "looks wrong is evidence, not permission" in replanner
    assert "inspect git history" in replanner
    assert "outside-scope missing-module stop signal" in replanner
    assert "benchmark test import as non-production evidence" in replanner
    assert "submit_replan(new_tasks=[], cancel_ids=[])" in replanner
    assert "do not call CI, file, graph, note, or CodeAct tools afterward" in replanner
    assert "Must not convert a coordinated write-tool failure into instructions to bypass coordination" in replanner
    assert "standard Python file I/O" in replanner
    assert "Never submit a corrective task with `*/tests/*`" in replanner

    assert "must load `completion-contract`" in scout.lower()
    assert "must not edit files" in scout.lower()
    assert "must keep missing targets missing" in scout.lower()
    assert "benchmark test target path as off-policy" in scout
    assert "do not locate or correct the test path" in scout
    assert "planner should scout the production owner path instead" in scout
    assert "no-symbol exact file should not be used as `scope_paths`" in scout
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
    developer_widening = _read(
        _CONTENT / "team-developer-playbook/references/widening-and-runtime.md"
    )
    scout_ref = _read(_CONTENT / "team-scout-playbook/references/completion-contract.md")
    validator_ref = _read(
        _CONTENT / "team-validator-playbook/references/runtime-verification-examples.md"
    )

    assert "Never keep a guessed exact leaf once live evidence disproves it." in planner_ref
    assert "structure listing that shows `pkg/io/foo/`" in planner_ref
    assert "read_task_note(paths=[...])` with default scope" in planner_ref
    assert "optional final helper" in planner_json
    assert "submit_plan(new_tasks=[...])" in planner_json
    assert "Do not include `task_note`" in planner_json
    assert "`1. Goal:`" in planner_json
    assert "Do not use Markdown headings" in planner_json
    assert "Mentioning dependencies inside `spec` does not set task deps" in planner_json
    assert "verification-only test targets in `spec` context or acceptance criteria" in planner_json
    assert "Missing modules, compatibility shims, re-export modules, and import bridges named only by tests" in planner_json
    assert "workspace structure shows a directory or nested files" in planner_json
    assert "child planners like `plan-parquet` or `plan-groupby`" in planner_json
    assert "Pairwise overlap check" in planner_json
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
    assert "Never use a scout to locate or correct a benchmark test path mismatch" in scout_launch
    assert "do not use scouts to repair benchmark test paths" in scout_launch
    assert "Never pass an exact file to a scout after a file-symbol query found no indexed symbols" in scout_launch
    assert "Never use scouts to locate or correct benchmark test path mismatches" in planner_ref
    assert 'daytona_codeact(command="...", timeout=N)' in developer_runtime
    assert "Must not append shell capture plumbing" in developer_runtime
    assert "Must not write or move files through CodeAct" in developer_runtime
    assert "Pure removals such as `rm`, `unlink`, `os.remove`" in developer_runtime
    assert "Must not inspect source through CodeAct" in developer_runtime
    assert "cd /testbed" in developer_playbook
    assert "cd /testbed" in developer_runtime
    assert "pkg._compat" in developer_root_cause
    assert "missing module, compatibility shim, re-export module, or import bridge" in developer_widening
    assert "not permission to create it" in developer_widening
    assert "scope_paths` itself names an absent module" in developer_widening
    assert "source and destination are separate ownership checks" in developer_widening
    assert "in-scope source file does not authorize an absent outside-scope destination path" in developer_widening
    assert "ModuleNotFoundError" in developer_widening
    assert "Do not read tests, glob/grep for the module" in developer_widening
    assert "query symbols for the missing import" in developer_widening
    assert "similar in-scope compatibility module is not provenance" in developer_widening
    assert "needed to make tests collect" in developer_widening.lower()
    assert "do not attempt an out-of-scope edit or write" in developer_widening.lower()
    assert "do not read, inspect, continue verifying" in developer_widening.lower()
    assert "a missing outside-scope owner becomes replan evidence" in developer_widening
    assert "The Task Center note is the durable handoff." in scout_ref
    assert "Make exactly one `submit_task_note(...)` call" in scout_ref
    assert "assistant text with no `submit_task_note(...)` call" in scout_ref
    assert "the exact file should not be used as `scope_paths`" in scout_ref
    assert "target path is off-policy" in scout_ref
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
    assert "new-file, rename, move, shim, or re-export task" in add_tasks
    assert "Self-check `cancel_ids=[]`" in add_tasks
    assert "replacement creates, renames, moves, or re-exports a test-derived missing path" in cancel_redraft
    assert "even when the source file is in scope" in add_tasks
    assert "even when the source file is in scope" in cancel_redraft
    assert "standard re-export pattern" in add_tasks
    assert "similar in-scope compatibility filename is not an exception" in cancel_redraft
    assert "Never create, rename, move, or re-export a missing compatibility module" in corrective_fast_path
    assert "live parent package or in-scope source file is not enough" in corrective_fast_path
    assert "similarly named live modules, package aliases, or adjacent compatibility files" in corrective_fast_path
    assert "empty `submit_replan(new_tasks=[], cancel_ids=[])`" in corrective_fast_path
    assert "do not call CI, file, graph, note, or CodeAct tools" in add_tasks
    assert "do not call CI, file, graph, note, or CodeAct tools" in cancel_redraft
    assert "Do not add a developer task whose `scope_paths` are benchmark or verification tests" in add_tasks
    assert "not add a test-edit developer task" in add_tasks
    assert "no task scopes benchmark tests" in add_tasks
    assert "Do not replace a failed task with a benchmark-test edit task" in cancel_redraft
    assert "instead of a test-edit developer task" in cancel_redraft
    assert "no replacement scopes benchmark tests" in cancel_redraft
    assert "Never make a benchmark test file the corrective owner" in corrective_fast_path
    assert "raw-write workaround" in add_tasks
    assert "whole-file overwrite fallback instructions" in cancel_redraft

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
    assert "Must treat `daytona_codeact` as runtime-first" in sweevo
    assert "Pure removals such as `rm`, `unlink`, `os.remove`" in sweevo
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
    assert "Must never launch scouts on benchmark test paths" in sweevo
    assert "use scouts to locate or correct benchmark test paths" in sweevo
    assert "scout the production owner path instead" in sweevo
    assert "Must not derive an exact production file from benchmark filename resemblance alone" in sweevo
    assert "no-symbol exact file plus a live directory/nested-file structure result" in sweevo
    assert "Must use `read_task_note(paths=[...])` before opening source files" in sweevo
    assert "Must not use `daytona_codeact` for source inspection" in sweevo
    assert "Must treat scope-change notifications and `task_center_changed_since()` as freshness signals." in sweevo
    assert "workflow rules are prompt/playbook obligations" in sweevo
    assert "Must keep `scope_paths` as soft coordination hints" in sweevo
    assert "Must treat test-file writes as off-policy" in sweevo
    assert "Must treat any advisory outside-scope write as a tainted packet" in sweevo
    assert "The exact missing import path from tests does not grant permission" in sweevo
    assert "Must stop immediately when CodeAct, diagnostics, or pytest collection output names a missing outside-scope module" in sweevo
    assert "An in-scope source file does not authorize an outside-scope destination path" in sweevo
    assert "retrying the same delete/move tool" in sweevo
    assert "inspect git history" in sweevo
    assert "glob/grep for shims" in sweevo
    assert "multiple tests importing it" in sweevo
    assert "similar in-scope compatibility filename" in sweevo
    assert "scope_paths` alone is not enough for an absent test-derived path" in sweevo
    assert "standard re-export pattern" in sweevo
    assert "replanners must not convert that blocker into a benchmark-test edit task" in sweevo
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
