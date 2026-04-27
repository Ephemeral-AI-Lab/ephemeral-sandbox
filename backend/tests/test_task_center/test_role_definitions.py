"""Tests for role-local harness agent definitions."""

from __future__ import annotations

from importlib.resources import files

from task_center.harness_agents.evaluator.definition import (
    EVALUATOR,
    load_system_prompt as load_evaluator_prompt,
)
from task_center.harness_agents.executor.definition import (
    EXECUTOR,
    load_system_prompt as load_executor_prompt,
)
from task_center.harness_agents.planner.definition import (
    PLANNER,
    load_system_prompt as load_planner_prompt,
)


def test_executor_definition_loads_role_local_markdown() -> None:
    expected = files("task_center.harness_agents.executor").joinpath(
        "agent.md"
    ).read_text(encoding="utf-8")
    assert load_executor_prompt() == expected
    assert EXECUTOR.system_prompt == expected


def test_executor_prompt_frames_atomicity_as_working_hypothesis() -> None:
    prompt = load_executor_prompt()
    # Atomicity is a hypothesis revised by exploration, not a one-shot gate.
    assert "atomicity as a working hypothesis" in prompt
    assert "Thought → Action → Observation, in the ReAct sense" in prompt
    assert "not enforced per step" in prompt
    # The three named beats are the floor.
    assert "Beat 1 — Initial estimate" in prompt
    assert "Beat 2 — After exploration converges, before the first mutation" in prompt
    assert "Beat 3 — On surprise" in prompt
    # Beat 1 is allowed to be a hypothesis, not a commitment.
    assert "hypothesis, not a commitment" in prompt


def test_executor_prompt_anti_momentum_policy_blocks_sunk_cost() -> None:
    prompt = load_executor_prompt()
    # The policy that turns checkpoints into action.
    assert "Anti-momentum policy" in prompt
    assert "escalate on the\nnext tool boundary" in prompt
    assert "Do not finish the current edit cluster" in prompt
    assert "evidence for the planner" in prompt
    # Anti-rationalizations are named with their counter-argument.
    assert "Count surfaces, not themes" in prompt
    assert "Cross-surface scouting is request_plan, not run_subagent" in prompt


def test_executor_prompt_mode_table_lists_plan_handoff_first() -> None:
    prompt = load_executor_prompt()
    handoff_idx = prompt.index("| Plan handoff   | request_plan")
    success_idx = prompt.index("| Direct success | submit_task_success")
    assert handoff_idx < success_idx, (
        "Plan handoff must appear before Direct success in the Mode "
        "Decision Table so escalation is the default framing for composite work."
    )


def test_planner_definition_loads_role_local_markdown() -> None:
    expected = files("task_center.harness_agents.planner").joinpath(
        "agent.md"
    ).read_text(encoding="utf-8")
    assert load_planner_prompt() == expected
    assert PLANNER.system_prompt == expected


def test_evaluator_definition_loads_role_local_markdown() -> None:
    expected = files("task_center.harness_agents.evaluator").joinpath(
        "agent.md"
    ).read_text(encoding="utf-8")
    assert load_evaluator_prompt() == expected
    assert EVALUATOR.system_prompt == expected
