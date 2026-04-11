"""Quality checks for bundled team playbooks."""

from __future__ import annotations

import re
from pathlib import Path


_BACKEND_ROOT = Path(__file__).resolve().parents[2]
_CONTENT = _BACKEND_ROOT / "src/skills/bundled/content"
_PLAYBOOKS = [
    _CONTENT / "team-developer-playbook/SKILL.md",
    _CONTENT / "team-validator-playbook/SKILL.md",
    _CONTENT / "team-posthook-decision-playbook/SKILL.md",
    _CONTENT / "team-planner-playbook/SKILL.md",
    _CONTENT / "team-replanner-playbook/SKILL.md",
    _CONTENT / "team-scout-playbook/SKILL.md",
]
_ALL_SKILLS = _PLAYBOOKS + [
    _CONTENT / "sweevo-project-context/SKILL.md",
    _CONTENT / "verification-replan/SKILL.md",
]
_REFERENCES = [
    _CONTENT / "team-developer-playbook/references/codeact-runtime-examples.md",
    _CONTENT / "team-developer-playbook/references/widening-and-runtime.md",
    _CONTENT / "team-planner-playbook/references/exploration-script.md",
    _CONTENT / "team-planner-playbook/references/scout-launch-contract.md",
    _CONTENT / "team-planner-playbook/references/non-root-context-reuse.md",
    _CONTENT / "team-planner-playbook/references/plan-json-contract.md",
    _CONTENT / "team-planner-playbook/references/task-planning-decomposition.md",
    _CONTENT / "team-scout-playbook/references/completion-contract.md",
    _CONTENT / "team-posthook-decision-playbook/references/decision-gates.md",
    _CONTENT / "team-validator-playbook/references/cross-surface-guardrails.md",
    _CONTENT / "team-replanner-playbook/references/corrective-fast-path.md",
    _CONTENT / "verification-replan/references/triage-format.md",
]


def _read(path: Path) -> str:
    return path.read_text(encoding="utf-8")


def _hard_rules_section(content: str) -> str:
    after_header = content.split("## Hard rules", 1)[1]
    return re.split(r"\n## ", after_header, maxsplit=1)[0]


def test_skills_and_references_stay_short() -> None:
    for path in _ALL_SKILLS:
        assert len(_read(path).splitlines()) <= 100, f"{path} should stay short"
    for path in _REFERENCES:
        assert len(_read(path).splitlines()) <= 60, f"{path} should stay short"


def test_hard_rule_numbers_do_not_repeat() -> None:
    for path in _PLAYBOOKS:
        section = _hard_rules_section(_read(path))
        labels = re.findall(r"^(\d+)\.\s", section, flags=re.MULTILINE)
        assert labels, f"expected numbered hard rules in {path}"
        assert labels == [str(i) for i in range(1, len(labels) + 1)], f"bad numbering in {path}"


def test_skills_use_clear_must_never_language() -> None:
    for path in _ALL_SKILLS + _REFERENCES:
        content = _read(path)
        assert "Must " in content or "Must\n" in content or "Must use" in content or "Must treat" in content
        assert "Never " in content or "Never\n" in content or "Never use" in content


def test_planner_skill_has_explicit_conditional_reference_loading() -> None:
    planner = _read(_CONTENT / "team-planner-playbook/SKILL.md")
    decomposition = _read(_CONTENT / "team-planner-playbook/references/task-planning-decomposition.md")
    exploration = _read(_CONTENT / "team-planner-playbook/references/exploration-script.md")
    non_root = _read(_CONTENT / "team-planner-playbook/references/non-root-context-reuse.md")
    plan_json = _read(_CONTENT / "team-planner-playbook/references/plan-json-contract.md")
    scout_launch = _read(_CONTENT / "team-planner-playbook/references/scout-launch-contract.md")
    assert "Fresh benchmark root: must load `exploration-script`" in planner
    assert "Immediately before final plan JSON: must load `plan-json-contract`" in planner
    assert "Fresh benchmark root: must load `task-planning-decomposition`" in planner
    assert "before loading `plan-json-contract` or `task-planning-decomposition`, must complete at least one scout wave" in planner
    assert "Child or `## Scoped Expansion` turn: must load `non-root-context-reuse`" in planner
    assert "Before the first scout wave: must load `scout-launch-contract`" in planner
    assert "when `load_skill_reference` is available" in planner
    assert "registered worker name in `agent_name`, human lane label in `local_id`" in planner
    assert "Must keep dependency local ids in the top-level `deps` field" in planner
    assert "Must emit each final lane exactly once." in planner
    assert "Atlas is cross-run memory only." in planner
    assert "the sequence is `anchor -> scout wave -> decomposition -> plan JSON`" in planner
    assert "Must use `agent_name` only for registered workers: `developer`, `validator`, or `team_planner`." in plan_json
    assert "Must keep `deps` as a top-level item field." in plan_json
    assert "Must emit each `local_id` only once." in plan_json
    assert "Do not emit `agent_name: \"fix_compat_make_bytes_tuple_version\"` or `kind: \"developer\"`." in plan_json
    assert "Do not merge `json.py`, `cli.py`, `config.py`, `compatibility.py`, and `utils.py` into one atomic `core_misc_fix` developer lane." in plan_json
    assert "Do not create one atomic \"misc fixes\" lane just because those residual slices are individually small." in decomposition
    assert "Do not collapse those unrelated files into one atomic developer just to save root-plan slots." in decomposition
    assert "Must emit a direct developer lane when the child turn already owns one exact production file" in non_root
    assert "Do not emit another `team_planner` child for the same single-file residual." in non_root
    assert "Never map a benchmark cluster to a production file solely because the names look similar." in exploration
    assert "the next planning action must be a scout wave, not final DAG synthesis" in exploration
    assert "run_subagent(agent_name=\"scout\", input={\"target_paths\":[\"pkg/io/parquet\"]}" in exploration
    assert 'Must call `run_subagent(agent_name="scout", input={"target_paths": [...]}, task_note="...")`.' in scout_launch
    assert "Never pass prompt mode to `scout`." in scout_launch


def test_replanner_skill_has_explicit_conditional_reference_loading() -> None:
    replanner = _read(_CONTENT / "team-replanner-playbook/SKILL.md")
    reference = _read(_CONTENT / "team-replanner-playbook/references/corrective-fast-path.md")
    assert "must load `corrective-fast-path` before deeper analysis" in replanner
    assert "when `load_skill_reference` is available" in replanner
    assert "Must start with `ci_scoped_status(scope_paths=[...])`" in reference


def test_developer_and_validator_skills_explain_when_to_load_references() -> None:
    developer = _read(_CONTENT / "team-developer-playbook/SKILL.md")
    developer_codeact_ref = _read(_CONTENT / "team-developer-playbook/references/codeact-runtime-examples.md")
    developer_ref = _read(_CONTENT / "team-developer-playbook/references/widening-and-runtime.md")
    validator = _read(_CONTENT / "team-validator-playbook/SKILL.md")
    validator_ref = _read(_CONTENT / "team-validator-playbook/references/cross-surface-guardrails.md")

    assert "Must load `widening-and-runtime` before the first widened write outside `owned_files`." in developer
    assert "Must load `widening-and-runtime` before concluding a runtime-owned lane from non-runtime evidence." in developer
    assert "Must load `codeact-runtime-examples` before the first `daytona_codeact` verification or reproduction command on a benchmark lane." in developer
    assert "Must use `daytona_codeact` for bounded runtime reproduction or verification." in developer
    assert "Must drive repo commands inside `daytona_codeact` through the provided `shell(\"...\")` helper." in developer
    assert "Never use `daytona_bash` from developer lanes." in developer
    assert "Must not use raw Python `subprocess.run(...)` snippets" in developer
    assert "Must execute repo commands through `shell(\"...\")` inside `daytona_codeact`." in developer_codeact_ref
    assert "Do not start a generic `pip install ...` loop" in developer_codeact_ref
    assert "Use this reference only when either condition is true:" in developer_ref

    assert "Must load `cross-surface-guardrails` when the touched change affects public serialization, schema shape, or docs-visible output." in validator
    assert "Must run the exact commands from the payload first via `daytona_codeact`." in validator
    assert "Never use `daytona_bash` from validator lanes." in validator
    assert "Use this reference only when the touched change affects public serialization, schema shape, or docs-visible output." in validator_ref


def test_posthook_and_verification_replan_explain_when_to_load_references() -> None:
    posthook = _read(_CONTENT / "team-posthook-decision-playbook/SKILL.md")
    posthook_ref = _read(_CONTENT / "team-posthook-decision-playbook/references/decision-gates.md")
    replan = _read(_CONTENT / "verification-replan/SKILL.md")
    replan_ref = _read(_CONTENT / "verification-replan/references/triage-format.md")

    assert "Must load `decision-gates` when the worker output is malformed" in posthook
    assert "Use this reference only when the worker output is malformed" in posthook_ref

    assert "Must load `triage-format` when you need to produce a manual FAIL summary" in replan
    assert "Use this reference only when you need a manual FAIL summary" in replan_ref


def test_scout_playbook_keeps_missing_targets_missing() -> None:
    scout = _read(_CONTENT / "team-scout-playbook/SKILL.md")
    scout_ref = _read(_CONTENT / "team-scout-playbook/references/completion-contract.md")
    assert "must keep that exact path missing" in scout
    assert "Never inspect nearby replacements" in scout
    assert "Must load `completion-contract` when `target_paths` is a single file" in scout
    assert "Never claim code was created, fixed, patched, or refactored." in scout
    assert "For single-file or short fixed file-list scouts, `suggested_subdivisions` should almost always be `[]`." in scout
    assert "Must treat the handed scope itself as the deliverable." in scout_ref
    assert "Never subdivide a single file just because it is long" in scout_ref


def test_sweevo_context_stays_shared_and_runtime_focused() -> None:
    sweevo = _read(_CONTENT / "sweevo-project-context/SKILL.md")
    assert "Must report a missing named test or node as `benchmark_surface_mismatch`." in sweevo
    assert "Must not label a missing transitive import, helper, or adjacent production module as `benchmark_surface_mismatch`" in sweevo
    assert "Must keep commands repo-root-relative." in sweevo
    assert "Must keep roles separate" in sweevo


def test_posthook_decision_playbook_forbids_clarifying_questions() -> None:
    posthook = _read(_CONTENT / "team-posthook-decision-playbook/SKILL.md")
    assert "Must not ask clarifying questions." in posthook
    assert "Must choose `summary`, `retry`, or `replan`" in posthook


def test_worker_playbooks_do_not_mention_submitters_or_action_routing() -> None:
    for path in (
        _CONTENT / "team-developer-playbook/SKILL.md",
        _CONTENT / "team-validator-playbook/SKILL.md",
        _CONTENT / "sweevo-project-context/SKILL.md",
    ):
        content = _read(path)
        assert "submit_summary" not in content
        assert "submit_replan" not in content
        assert "RECOMMENDED_ACTION" not in content
