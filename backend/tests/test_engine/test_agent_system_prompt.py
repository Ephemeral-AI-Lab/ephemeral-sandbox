from __future__ import annotations

from types import SimpleNamespace

from agents.types import AgentDefinition
from engine.runtime import agent as runtime_agent


def test_declared_skills_are_prepended_before_base_prompt(monkeypatch):
    monkeypatch.setattr(
        runtime_agent,
        "_build_declared_skill_preamble",
        lambda *_args, **_kwargs: "# Preloaded Skills\n\nskill body",
    )

    prompt = runtime_agent._build_agent_system_prompt(
        SimpleNamespace(cwd="/tmp"),
        AgentDefinition(name="planner", description="d", system_prompt="base prompt"),
        settings=None,
        latest_user_prompt=None,
    )

    assert prompt.startswith("# Preloaded Skills")
    assert prompt.endswith("base prompt")
