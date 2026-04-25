from __future__ import annotations

from types import SimpleNamespace

from agents.types import AgentDefinition
from engine.runtime import agent as runtime_agent


def test_agent_system_prompt_includes_runtime_base_and_agent_body_only(monkeypatch):
    monkeypatch.setattr(
        runtime_agent,
        "build_runtime_system_prompt",
        lambda *_args, **_kwargs: "runtime base",
    )

    prompt = runtime_agent._build_agent_system_prompt(
        SimpleNamespace(cwd="/tmp"),
        AgentDefinition(name="planner", description="d", system_prompt="base prompt"),
        settings=None,
        latest_user_prompt=None,
    )

    assert prompt.startswith("runtime base")
    assert "base prompt" in prompt
    assert "# Declared Skills" not in prompt
    assert "# Identity" not in prompt
    assert "# Type Constraints" not in prompt
    assert "# Role Boundary" not in prompt
    assert "# Skill Bootstrap" not in prompt


def test_agent_system_prompt_ignores_declared_skills(monkeypatch) -> None:
    monkeypatch.setattr(
        runtime_agent,
        "build_runtime_system_prompt",
        lambda *_args, **_kwargs: "",
    )
    agent = AgentDefinition(
        name="minimal",
        description="d",
        system_prompt="agent body",
        skills=["demo-skill"],
        include_skills=True,
    )

    prompt = runtime_agent._build_agent_system_prompt(
        SimpleNamespace(cwd="/tmp"),
        agent,
        settings=None,
        latest_user_prompt=None,
    )

    assert prompt == "agent body"

