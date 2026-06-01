"""Tests for repository agent markdown definitions."""

from __future__ import annotations

from pathlib import Path

from agents import AgentRole, AgentType, load_agents_dir, load_agents_tree


BACKEND_ROOT = Path(__file__).resolve().parents[3]
AGENTS_ROOT = BACKEND_ROOT / "src" / "agents"
MAIN_PROFILE_DIR = AGENTS_ROOT / "profile" / "main"
SUBAGENT_PROFILE_DIR = AGENTS_ROOT / "profile" / "subagent"


def _load_named(directory: Path, name: str):
    loaded = load_agents_dir(directory)
    by_name = {a.name: a for a in loaded}
    assert name in by_name, f"agent {name!r} not found in {directory}"
    return by_name[name]


def test_harness_agent_markdown_declares_notification_triggers() -> None:
    planner = _load_named(MAIN_PROFILE_DIR, "planner")
    executor = _load_named(MAIN_PROFILE_DIR, "executor")
    reducer = _load_named(MAIN_PROFILE_DIR, "reducer")

    assert planner.notification_triggers == ["nested_planner_deferral_disabled"]
    assert executor.notification_triggers == []
    assert reducer.notification_triggers == []


def test_recursive_agent_loader_finds_harness_profiles() -> None:
    loaded = load_agents_tree(MAIN_PROFILE_DIR)
    by_name = {agent.name: agent for agent in loaded}

    assert {
        "planner",
        "executor",
        "reducer",
    } <= set(by_name)
    assert by_name["executor"].role == AgentRole.GENERATOR
    assert by_name["executor"].agent_type == AgentType.AGENT
    assert by_name["executor"].terminals == ["submit_generator_outcome"]
    assert "delegate_workflow" in by_name["executor"].allowed_tools


def test_executor_profile_uses_goal_solution_terminal() -> None:
    executor = _load_named(MAIN_PROFILE_DIR, "executor")

    assert "submit_generator_outcome" in executor.terminals
    assert "delegate_workflow" in executor.allowed_tools
    assert "ask_resolver" not in executor.allowed_tools


def test_subagent_profile_uses_subagent_agent_type() -> None:
    explorer = _load_named(SUBAGENT_PROFILE_DIR, "explorer")

    assert explorer.agent_type == AgentType.SUBAGENT
