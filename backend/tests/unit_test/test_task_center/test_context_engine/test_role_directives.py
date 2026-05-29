"""Coverage for the ROLE_DIRECTIVES registry."""

from __future__ import annotations

from task_center.agent_launch.task_guidance_dispatch import (
    _AGENTS_WITH_TASK_GUIDANCE,
    task_guidance_builder_for,
)
from task_center.context_engine.role_directives import ROLE_DIRECTIVES


def test_every_dispatched_agent_has_a_directive():
    """An agent that routes through the registry-driven builder must carry a
    directive — the builder raises otherwise."""
    missing = sorted(
        name for name in _AGENTS_WITH_TASK_GUIDANCE if name not in ROLE_DIRECTIVES
    )
    assert missing == [], f"agents missing ROLE_DIRECTIVES: {missing}"


def test_directives_match_spec_lines():
    expected = {
        "planner": "Plan for <iteration_goal>.",
        "executor": "Complete <assigned_task>.",
        "verifier": "Complete <assigned_task>.",
        "evaluator": "Verify the current attempt against <evaluation_criteria>.",
        "explorer": (
            "Investigate the parent's question and return concrete findings."
        ),
    }
    for name, line in expected.items():
        assert ROLE_DIRECTIVES[name] == line


def test_unknown_agent_has_no_task_guidance_builder():
    assert task_guidance_builder_for("unknown-agent") is None
