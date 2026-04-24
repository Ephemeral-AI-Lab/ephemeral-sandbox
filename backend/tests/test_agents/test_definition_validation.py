"""Tests for agent definition validation and builtin-name reservation."""

from __future__ import annotations

import agents.registry as registry
from types import SimpleNamespace

from agents.builder.validation import AgentDefinitionValidator
from agents.registry import get_definition
from agents.types import AgentDefinition
from team.definitions import register_all as _register_team_builtins


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
            tools=None,
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

def test_tools_csv_split():
    defn = AgentDefinition(name="dev", description="dev", tools="ci_query_symbol, ci_diagnostics")
    assert defn.tools == ["ci_query_symbol", "ci_diagnostics"]


def test_builder_validation_allows_known_tools():
    validator = AgentDefinitionValidator(tool_registry=None)

    result = validator.validate(  # type: ignore[arg-type]
        SimpleNamespace(
            name="custom_agent",
            tools=["ci_query_symbol"],
            effort=None,
        )
    )

    assert result.valid is True
    assert result.errors == []


def test_builder_validation_rejects_unknown_tools():
    validator = AgentDefinitionValidator(tool_registry=None)

    result = validator.validate(  # type: ignore[arg-type]
        SimpleNamespace(
            name="custom_agent",
            tools=["does_not_exist"],
            effort=None,
        )
    )

    assert result.valid is False
    assert "Unknown tool: does_not_exist" in result.errors
