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
    assert 'A missing `class` hit from `ci_query_symbols(kind="class")` is not enough to conclude a public API is absent.' in planner
    assert 'Do not claim "class X is missing from the codebase" from planner-side symbol misses alone.' in planner
    assert "On fresh benchmark root turns, do **not** open with `atlas_lookup`." in planner
    assert "on fresh benchmark roots, use `ci_scope_status(...)` and fresh scouts before any atlas lookup" in planner
    assert 'If you plan to join `task_id="all"`, inspect each fresh scout in that batch first' in planner
    assert 'Never call `run_subagent` with `agent_name="team_planner"`' in planner
    assert "duplicate-scout rejection over an already mapped path is terminal planning evidence" in planner
    assert "If a downstream developer or validator would still need fresh ownership discovery to start" in planner
    assert "Every execution lane should also receive the minimal handoff packet it needs to start immediately" in planner
    assert "Retry/replan handoff packets must preserve clustered failures, affected files, and what changed since the last healthy checkpoint or validator pass." in planner
    assert "do not expect validator or developer lanes to rediscover the owner map with fresh repo-wide probing" in planner


def test_sweevo_context_treats_missing_share_briefing_as_non_blocking() -> None:
    sweevo = _read(_SWEEVO_CONTEXT)
    assert "should not spend tool budget on explicit `share_briefing` promotion unless that tool is visibly available" in sweevo
    assert "treat that as a no-promotion profile, not as a blocker" in sweevo
    assert "representative deduped subset of failing ids" in sweevo
    assert "repeat `local_id`, `agent_name`, `kind`, or `payload` keys inside one JSON object" in sweevo
    assert 'A planner-side `ci_query_symbols(kind="class")` miss does not prove a public type is absent from the repo.' in sweevo
    assert "After a bounded export fix, rerun the named pytest entry point before widening the same lane to additional public names." in sweevo
    assert "Once that missing public name is anchored to a local export file, do not spend developer budget on dependency version checks" in sweevo
    assert "Fresh benchmark roots should stay live-first." in sweevo
    assert "prefer `ci_scope_status(scope_paths=[...])` plus fresh scouts over `atlas_lookup`" in sweevo
    assert "Retry/replan handoff must preserve the evidence packet." in sweevo
    assert "Ownership mismatch is a planning problem." in sweevo
    assert "Planner briefings must be execution-ready." in sweevo
    assert "Do not push that rediscovery work down to the next developer or validator lane." in sweevo
    assert "Preserve exact pytest node ids verbatim in planner payloads." in sweevo
    assert "Do not shorten `test_info_versions` to `test_info`" in sweevo
    assert 'Do not "repair" the benchmark by editing the unowned test file' in sweevo


def test_developer_playbook_anchors_import_failures_to_named_pytest_surface() -> None:
    developer = _read(_BACKEND_ROOT / "src/skills/bundled/content/team-developer-playbook/SKILL.md")
    assert "If that first entry point is an import or collection failure" in developer
    assert "Do not promote a probe-only theory into broader code edits" in developer
    assert 'A `ci_query_symbols(kind="class")` miss is not proof that a public type is absent.' in developer
    assert "When the first pytest failure is a missing public name" in developer
    assert "After fixing one missing export or public name, rerun the named pytest entry point before adding any other symbols." in developer
    assert "inspect the package export bridge next" in developer
    assert "exact failing import path succeeds in a fresh Python process" in developer
    assert "In coordinated team developer lanes, `daytona_codeact` is intentionally unavailable." in developer
    assert "Do not escalate a surgical same-file export or alias fix into `daytona_codeact`." in developer
    assert "After a targeted retest fails, re-read the edited block before writing custom debug scripts." in developer
    assert "Budget warnings require the identified patch point, not more diagnosis." in developer
    assert "Rejected mutating shell probes are a stop sign." in developer
    assert "patch the last merge/update function that overwrites the public field" in developer
    assert "If the first failing pytest surface is inside an unowned test file" in developer
    assert "Named-node mismatches are not permission to rewrite tests." in developer


def test_validator_playbook_mentions_codeact_is_unavailable_in_team_lanes() -> None:
    validator = _read(_BACKEND_ROOT / "src/skills/bundled/content/team-validator-playbook/SKILL.md")
    assert "coordinated team validation lanes intentionally omit `daytona_codeact`" in validator
    assert "Ownership mismatch is not a validator discovery task." in validator
    assert "report `FAILURE_TYPE: plan_gap` and `RECOMMENDED_ACTION: request_replan`." in validator
    assert "Validators are not backup planners." in validator


def test_posthook_decision_playbook_forbids_clarifying_questions_on_worker_output() -> None:
    posthook = _read(_BACKEND_ROOT / "src/skills/bundled/content/team-posthook-decision-playbook/SKILL.md")
    assert "Every incoming message is worker output from the previous phase" in posthook
    assert "Do not ask clarifying questions." in posthook
    assert "Malformed worker output still requires a decision." in posthook
