from __future__ import annotations

from types import SimpleNamespace

import pytest

from agents import AgentDefinition, AgentKind
from engine.agent import factory as runtime_agent


_MAIN_ROLE_BASE_HEADER = "# Main-Agent Operating Contract"


def _stub_runtime_base(monkeypatch, text: str = "runtime base") -> None:
    monkeypatch.setattr(
        runtime_agent,
        "build_runtime_system_prompt",
        lambda *_args, **_kwargs: text,
    )


def test_agent_system_prompt_includes_runtime_base_and_agent_body_only(monkeypatch):
    _stub_runtime_base(monkeypatch)

    prompt = runtime_agent._build_agent_system_prompt(
        SimpleNamespace(cwd="/tmp"),
        AgentDefinition(
            name="planner",
            description="d",
            agent_kind=AgentKind.PLANNER,
            system_prompt="base prompt",
        ),
        settings=None,
    )

    assert prompt.startswith("runtime base")
    assert "base prompt" in prompt
    assert "# Declared Skills" not in prompt
    assert "# Identity" not in prompt
    assert "# Type Constraints" not in prompt
    assert "# Role Boundary" not in prompt
    assert "# Skill Bootstrap" not in prompt


@pytest.mark.parametrize(
    ("name", "kind"),
    [
        ("advisor", AgentKind.ADVISOR),
        ("explorer", AgentKind.EXPLORER),
    ],
)
def test_main_role_base_not_injected_at_runtime(monkeypatch, name, kind):
    """Post-v3.3: the main-role operating contract is prepended at
    agent-definition LOAD time (see ``agents/profile/main/_main_role_contract.md``
    + ``agents/definition/loader.py``), not at runtime. When the test builds
    an ``AgentDefinition`` directly with ``system_prompt="role body"``, the
    runtime factory MUST NOT re-inject the contract on top.
    """
    _stub_runtime_base(monkeypatch)

    prompt = runtime_agent._build_agent_system_prompt(
        SimpleNamespace(cwd="/tmp"),
        AgentDefinition(
            name=name,
            description="d",
            agent_kind=kind,
            system_prompt="role body",
        ),
        settings=None,
    )

    assert _MAIN_ROLE_BASE_HEADER not in prompt


def test_main_role_base_excluded_for_subagent(monkeypatch):
    _stub_runtime_base(monkeypatch)

    prompt = runtime_agent._build_agent_system_prompt(
        SimpleNamespace(cwd="/tmp"),
        AgentDefinition(
            name="explorer",
            description="d",
            agent_kind=AgentKind.EXPLORER,
            agent_type="subagent",
            system_prompt="role body",
        ),
        settings=None,
    )

    assert _MAIN_ROLE_BASE_HEADER not in prompt
