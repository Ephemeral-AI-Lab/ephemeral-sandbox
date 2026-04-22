"""Tests for agent definition validation and builtin-name reservation."""

from __future__ import annotations

import agents.registry as registry
from types import SimpleNamespace

from agents.builder.validation import AgentDefinitionValidator
from agents.registry import get_definition
from agents.types import AgentDefinition
from team.builtins import register_all as _register_team_builtins


if get_definition("team_planner") is None:
    try:
        _register_team_builtins()
    except Exception:
        pass


def test_builder_validation_rejects_reserved_builtin_agent_names():
    validator = AgentDefinitionValidator(tool_registry=None)

    result = validator.validate(  # type: ignore[arg-type]
        SimpleNamespace(
            name="team_planner",
            toolkits=None,
            effort=None,
        )
    )

    assert result.valid is False
    assert any("reserved for a builtin runtime agent" in err for err in result.errors)


def test_reserved_builtin_agent_names_match_current_team_runtime():
    assert registry.RESERVED_BUILTIN_AGENT_NAMES == {
        "root_planner",
        "team_planner",
        "developer",
        "validator",
        "scout",
        "team_replanner",
        "note_taker",
        "parent_summarizer",
    }


def test_registry_ignores_external_reserved_builtin_overrides(monkeypatch):
    monkeypatch.setattr(registry, "_external_loaded", False)
    monkeypatch.setattr(
        "agents.loader.load_external_agents",
        lambda: [
            AgentDefinition(
                name="team_planner",
                description="bad override",
                agent_type="subagent",
                source="user",
            )
        ],
    )

    planner = registry.get_definition("team_planner")

    assert planner is not None
    assert planner.source == "builtin"
    assert planner.agent_type == "agent"


def test_allowed_triggers_defaults_empty():
    defn = AgentDefinition(name="test_agent", description="test")
    assert defn.allowed_triggers == []


def test_allowed_triggers_from_list():
    defn = AgentDefinition(name="dev", description="dev", allowed_triggers=["tc_note"])
    assert defn.allowed_triggers == ["tc_note"]


def test_allowed_triggers_csv_split():
    defn = AgentDefinition(name="dev", description="dev", allowed_triggers="tc_note, future_trigger")
    assert defn.allowed_triggers == ["tc_note", "future_trigger"]


def test_allowed_tools_csv_split():
    defn = AgentDefinition(name="dev", description="dev", allowed_tools="ci_query_symbol, ci_diagnostics")
    assert defn.allowed_tools == ["ci_query_symbol", "ci_diagnostics"]


def test_builder_validation_allows_global_allowed_tools_without_toolkits():
    validator = AgentDefinitionValidator(tool_registry=None)

    result = validator.validate(  # type: ignore[arg-type]
        SimpleNamespace(
            name="custom_agent",
            toolkits=None,
            allowed_tools=["ci_query_symbol"],
            effort=None,
        )
    )

    assert result.valid is True
    assert result.errors == []


def test_builder_validation_rejects_unknown_allowed_tools():
    validator = AgentDefinitionValidator(tool_registry=None)

    result = validator.validate(  # type: ignore[arg-type]
        SimpleNamespace(
            name="custom_agent",
            toolkits=["code_intelligence"],
            allowed_tools=["does_not_exist"],
            effort=None,
        )
    )

    assert result.valid is False
    assert "Unknown allowed tool: does_not_exist" in result.errors
