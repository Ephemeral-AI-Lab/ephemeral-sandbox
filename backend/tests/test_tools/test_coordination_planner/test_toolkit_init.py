"""Unit tests for CoordinationPlannerToolkit (__init__.py)."""

from __future__ import annotations

import pytest

from tools.coordination_planner import CoordinationPlannerToolkit


class TestCoordinationPlannerToolkitInit:
    def test_instantiates_with_no_args(self) -> None:
        tk = CoordinationPlannerToolkit()
        assert tk is not None

    def test_name_is_coordination_planner(self) -> None:
        tk = CoordinationPlannerToolkit()
        assert tk.name == "coordination_planner"

    def test_description_is_set(self) -> None:
        tk = CoordinationPlannerToolkit()
        assert tk.description

    def test_instructions_is_set(self) -> None:
        tk = CoordinationPlannerToolkit()
        assert tk.instructions

    def test_registers_three_tools(self) -> None:
        tk = CoordinationPlannerToolkit()
        assert len(tk.list_tools()) == 3

    def test_registers_list_agents_tool(self) -> None:
        tk = CoordinationPlannerToolkit()
        assert tk.get("list_agents") is not None

    def test_registers_query_phase_context_tool(self) -> None:
        tk = CoordinationPlannerToolkit()
        assert tk.get("query_phase_context") is not None

    def test_registers_list_phases_tool(self) -> None:
        tk = CoordinationPlannerToolkit()
        assert tk.get("list_phases") is not None

    def test_agent_names_forwarded(self) -> None:
        tk = CoordinationPlannerToolkit(agent_names=["a", "b"])
        assert tk.get("list_agents") is not None

    def test_phase_outputs_forwarded(self) -> None:
        tk = CoordinationPlannerToolkit(phase_outputs={"p1": {"k": "v"}})
        assert tk.get("query_phase_context") is not None
        assert tk.get("list_phases") is not None

    def test_none_agent_names_accepted(self) -> None:
        tk = CoordinationPlannerToolkit(agent_names=None)
        assert len(tk.list_tools()) == 3

    def test_none_phase_outputs_accepted(self) -> None:
        tk = CoordinationPlannerToolkit(phase_outputs=None)
        assert len(tk.list_tools()) == 3

    def test_tool_names_returns_expected_set(self) -> None:
        tk = CoordinationPlannerToolkit()
        assert set(tk.tool_names()) == {"list_agents", "query_phase_context", "list_phases"}

    def test_all_exported(self) -> None:
        from tools.coordination_planner import __all__
        assert "CoordinationPlannerToolkit" in __all__
