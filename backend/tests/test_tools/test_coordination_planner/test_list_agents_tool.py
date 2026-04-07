"""Unit tests for coordination_planner list_agents_tool."""

from __future__ import annotations

import json
from pathlib import Path
from unittest.mock import MagicMock

import pytest

from tools.coordination_planner.list_agents_tool import make_list_agents_tool
from tools.core.base import ToolExecutionContext


def _ctx(metadata: dict | None = None) -> ToolExecutionContext:
    return ToolExecutionContext(cwd=Path("/tmp"), metadata=metadata or {})


def _make_get_metadata(agents: dict[str, dict]):
    """Return a get_agent_metadata callable backed by a static dict."""
    def get_agent_metadata(name: str) -> dict:
        if name not in agents:
            raise KeyError(f"Unknown agent: {name}")
        return agents[name]
    return get_agent_metadata


# ---------------------------------------------------------------------------
# No metadata service
# ---------------------------------------------------------------------------


class TestListAgentsNoMetadataService:
    async def test_returns_error_when_no_get_metadata(self) -> None:
        tool = make_list_agents_tool(agent_names=["alpha"])
        ctx = _ctx({})  # no get_agent_metadata key
        result = await tool.execute(tool.input_model(), ctx)
        assert result.is_error
        payload = json.loads(result.output)
        assert "error" in payload

    async def test_error_message_mentions_not_available(self) -> None:
        tool = make_list_agents_tool()
        ctx = _ctx()
        result = await tool.execute(tool.input_model(), ctx)
        assert result.is_error
        assert "not available" in result.output


# ---------------------------------------------------------------------------
# agent_names provided via closure
# ---------------------------------------------------------------------------


class TestListAgentsWithClosureNames:
    def _agents(self):
        return {
            "alpha": {"name": "alpha", "description": "Alpha agent", "role": "worker"},
            "beta": {"name": "beta", "description": "Beta agent", "role": "planner"},
            "gamma": {"name": "gamma", "description": "Gamma agent", "role": "worker"},
        }

    async def test_returns_all_agents_when_no_filter(self) -> None:
        tool = make_list_agents_tool(agent_names=["alpha", "beta", "gamma"])
        ctx = _ctx({"get_agent_metadata": _make_get_metadata(self._agents())})
        result = await tool.execute(tool.input_model(), ctx)
        assert not result.is_error
        agents = json.loads(result.output)
        assert len(agents) == 3
        names = {a["name"] for a in agents}
        assert names == {"alpha", "beta", "gamma"}

    async def test_role_filter_returns_only_matching(self) -> None:
        tool = make_list_agents_tool(agent_names=["alpha", "beta", "gamma"])
        ctx = _ctx({"get_agent_metadata": _make_get_metadata(self._agents())})
        result = await tool.execute(tool.input_model(role_filter="worker"), ctx)
        assert not result.is_error
        agents = json.loads(result.output)
        assert len(agents) == 2
        assert all(a["role"] == "worker" for a in agents)

    async def test_role_filter_no_match_returns_empty_list(self) -> None:
        tool = make_list_agents_tool(agent_names=["alpha", "beta"])
        ctx = _ctx({"get_agent_metadata": _make_get_metadata(self._agents())})
        result = await tool.execute(tool.input_model(role_filter="nonexistent"), ctx)
        assert not result.is_error
        agents = json.loads(result.output)
        assert agents == []

    async def test_exclude_roles_removes_matching(self) -> None:
        tool = make_list_agents_tool(agent_names=["alpha", "beta", "gamma"])
        ctx = _ctx({"get_agent_metadata": _make_get_metadata(self._agents())})
        result = await tool.execute(
            tool.input_model(exclude_roles=["planner"]), ctx
        )
        assert not result.is_error
        agents = json.loads(result.output)
        assert len(agents) == 2
        assert all(a["role"] != "planner" for a in agents)

    async def test_exclude_roles_multiple(self) -> None:
        tool = make_list_agents_tool(agent_names=["alpha", "beta", "gamma"])
        ctx = _ctx({"get_agent_metadata": _make_get_metadata(self._agents())})
        result = await tool.execute(
            tool.input_model(exclude_roles=["worker", "planner"]), ctx
        )
        assert not result.is_error
        assert json.loads(result.output) == []

    async def test_empty_agent_names_returns_empty_list(self) -> None:
        tool = make_list_agents_tool(agent_names=[])
        ctx = _ctx({"get_agent_metadata": _make_get_metadata(self._agents())})
        result = await tool.execute(tool.input_model(), ctx)
        assert not result.is_error
        assert json.loads(result.output) == []

    async def test_result_shape_has_expected_keys(self) -> None:
        tool = make_list_agents_tool(agent_names=["alpha"])
        ctx = _ctx({"get_agent_metadata": _make_get_metadata(self._agents())})
        result = await tool.execute(tool.input_model(), ctx)
        agent = json.loads(result.output)[0]
        assert set(agent.keys()) >= {"name", "description", "role"}


# ---------------------------------------------------------------------------
# agent_names=None — falls back to list_agents fn from context
# ---------------------------------------------------------------------------


class TestListAgentsFromContextFn:
    async def test_uses_list_agents_fn_when_no_closure_names(self) -> None:
        agents_data = {
            "x": {"name": "x", "description": "X", "role": "worker"},
            "y": {"name": "y", "description": "Y", "role": "worker"},
        }
        list_fn = MagicMock(return_value=["x", "y"])
        tool = make_list_agents_tool(agent_names=None)
        ctx = _ctx({
            "get_agent_metadata": _make_get_metadata(agents_data),
            "list_agents": list_fn,
        })
        result = await tool.execute(tool.input_model(), ctx)
        assert not result.is_error
        list_fn.assert_called_once()
        agents = json.loads(result.output)
        assert len(agents) == 2

    async def test_no_list_agents_fn_and_no_closure_returns_empty(self) -> None:
        agents_data = {"x": {"name": "x", "description": "X", "role": "worker"}}
        tool = make_list_agents_tool(agent_names=None)
        ctx = _ctx({"get_agent_metadata": _make_get_metadata(agents_data)})
        result = await tool.execute(tool.input_model(), ctx)
        assert not result.is_error
        assert json.loads(result.output) == []


# ---------------------------------------------------------------------------
# Metadata fetch errors are logged and skipped
# ---------------------------------------------------------------------------


class TestListAgentsMetadataErrors:
    async def test_failing_metadata_skips_agent(self) -> None:
        def bad_metadata(name: str) -> dict:
            raise RuntimeError("boom")

        tool = make_list_agents_tool(agent_names=["alpha"])
        ctx = _ctx({"get_agent_metadata": bad_metadata})
        result = await tool.execute(tool.input_model(), ctx)
        assert not result.is_error
        # Agent that raised is silently skipped
        assert json.loads(result.output) == []

    async def test_partial_failure_skips_only_failing(self) -> None:
        agents_data = {
            "good": {"name": "good", "description": "ok", "role": "worker"},
        }

        def mixed_metadata(name: str) -> dict:
            if name == "bad":
                raise ValueError("fail")
            return agents_data[name]

        tool = make_list_agents_tool(agent_names=["good", "bad"])
        ctx = _ctx({"get_agent_metadata": mixed_metadata})
        result = await tool.execute(tool.input_model(), ctx)
        assert not result.is_error
        agents = json.loads(result.output)
        assert len(agents) == 1
        assert agents[0]["name"] == "good"


# ---------------------------------------------------------------------------
# Tool metadata
# ---------------------------------------------------------------------------


class TestListAgentsToolMetadata:
    def test_tool_name(self) -> None:
        tool = make_list_agents_tool()
        assert tool.name == "list_agents"

    def test_tool_description_is_set(self) -> None:
        tool = make_list_agents_tool()
        assert tool.description

    def test_input_model_has_role_filter_field(self) -> None:
        tool = make_list_agents_tool()
        schema = tool.input_model.model_json_schema()
        assert "role_filter" in schema.get("properties", {})

    def test_input_model_has_exclude_roles_field(self) -> None:
        tool = make_list_agents_tool()
        schema = tool.input_model.model_json_schema()
        assert "exclude_roles" in schema.get("properties", {})
