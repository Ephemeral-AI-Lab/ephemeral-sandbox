"""US-015: planner_full_only agent.md drift + frontmatter assertions."""

from __future__ import annotations

from pathlib import Path

from agents.loader import load_agents_dir

REPO_ROOT = Path(__file__).resolve().parents[3]
PLANNER_DIR = REPO_ROOT / "backend" / "src" / "agents" / "main_agent" / "planner"


def _load_planner_pair():
    loaded = load_agents_dir(PLANNER_DIR)
    by_name = {agent.name: agent for agent in loaded}
    assert "planner" in by_name
    assert "planner_full_only" in by_name
    return by_name["planner"], by_name["planner_full_only"]


def test_both_planner_definitions_load():
    planner, full_only = _load_planner_pair()
    assert planner.role == "planner"
    assert full_only.role == "planner"


def test_full_only_terminals_exclude_partial_plan():
    _, full_only = _load_planner_pair()
    assert "submit_full_plan" in full_only.terminals
    assert "submit_partial_plan" not in full_only.terminals


def test_full_only_body_contains_no_partial_plan_prose():
    _, full_only = _load_planner_pair()
    body = full_only.system_prompt or ""
    assert "submit_partial_plan" not in body
    assert "continuation_goal" not in body


def test_full_only_has_no_variants():
    """Variant targets must not declare their own variants — chaining is forbidden."""
    _, full_only = _load_planner_pair()
    assert full_only.variants == []


def test_both_planners_share_planner_v1_recipe():
    planner, full_only = _load_planner_pair()
    assert planner.context_recipe == "planner_v1"
    assert full_only.context_recipe == "planner_v1"


def test_planner_variants_declare_full_only_target():
    planner, _ = _load_planner_pair()
    assert len(planner.variants) == 1
    variant = planner.variants[0]
    assert variant.when == "partial_plan_caller_ancestor"
    assert variant.use == "planner_full_only"
    assert variant.required_context_blocks == []


def test_planner_no_longer_lists_recursive_partial_plan_trigger():
    """The frontmatter terminals filter on planner_full_only is the gate now;
    the legacy notification trigger is removed (US-016)."""
    planner, _ = _load_planner_pair()
    assert "recursive_partial_plan" not in planner.notification_triggers
