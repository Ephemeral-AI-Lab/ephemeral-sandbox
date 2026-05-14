"""Tests for repository agent markdown definitions."""

from __future__ import annotations

from pathlib import Path

from agents import AgentKind, load_agents_dir, load_agents_tree


BACKEND_ROOT = Path(__file__).resolve().parents[3]
AGENTS_ROOT = BACKEND_ROOT / "src" / "agents"
MAIN_PROFILE_DIR = AGENTS_ROOT / "profile" / "main"


def _load_named(directory: Path, name: str):
    loaded = load_agents_dir(directory)
    by_name = {a.name: a for a in loaded}
    assert name in by_name, f"agent {name!r} not found in {directory}"
    return by_name[name]


def test_harness_agent_markdown_declares_notification_triggers() -> None:
    planner = _load_named(MAIN_PROFILE_DIR, "planner")
    executor = _load_named(MAIN_PROFILE_DIR, "executor")
    verifier = _load_named(MAIN_PROFILE_DIR, "verifier")
    evaluator = _load_named(MAIN_PROFILE_DIR, "evaluator")

    # The planner's recursive_partial_plan notification trigger was retired
    # in favour of the agent.md `terminals:` filter on planner_full_only —
    # if the variant fires, submit_partial_plan is never bound to the LLM
    # tool registry, so a soft reminder serves no purpose.
    assert planner.notification_triggers == []
    assert executor.notification_triggers == ["request_mission_after_edit"]
    assert verifier.notification_triggers == ["resolver_limit"]
    assert evaluator.notification_triggers == ["resolver_limit"]


def test_recursive_agent_loader_finds_harness_profiles() -> None:
    loaded = load_agents_tree(MAIN_PROFILE_DIR)
    by_name = {agent.name: agent for agent in loaded}

    assert {"planner", "executor", "verifier", "evaluator"} <= set(by_name)
    assert by_name["executor"].agent_kind == AgentKind.EXECUTOR
    assert "request_mission_solution" in by_name["executor"].terminals


def test_executor_agent_uses_mission_solution_terminal() -> None:
    executor = _load_named(MAIN_PROFILE_DIR, "executor")

    assert "request_mission_solution" in executor.terminals
    assert "ask_resolver" not in executor.allowed_tools
