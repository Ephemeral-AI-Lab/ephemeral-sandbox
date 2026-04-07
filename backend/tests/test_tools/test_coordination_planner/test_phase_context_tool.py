"""Unit tests for coordination_planner phase_context_tool."""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from tools.coordination_planner.phase_context_tool import (
    make_list_phases_tool,
    make_query_phase_context_tool,
)
from tools.core.base import ToolExecutionContext


def _ctx() -> ToolExecutionContext:
    return ToolExecutionContext(cwd=Path("/tmp"), metadata={})


# ---------------------------------------------------------------------------
# query_phase_context — missing phase
# ---------------------------------------------------------------------------


class TestQueryPhaseContextMissingPhase:
    async def test_missing_phase_returns_error(self) -> None:
        tool = make_query_phase_context_tool(phase_outputs={"p1": {"k": "v"}})
        result = await tool.execute(tool.input_model(phase="nope"), _ctx())
        assert result.is_error
        payload = json.loads(result.output)
        assert "error" in payload

    async def test_missing_phase_error_lists_available(self) -> None:
        tool = make_query_phase_context_tool(phase_outputs={"p1": {"k": "v"}})
        result = await tool.execute(tool.input_model(phase="missing"), _ctx())
        payload = json.loads(result.output)
        assert "p1" in payload.get("available_phases", [])

    async def test_empty_phase_outputs_returns_error(self) -> None:
        tool = make_query_phase_context_tool(phase_outputs={})
        result = await tool.execute(tool.input_model(phase="any"), _ctx())
        assert result.is_error

    async def test_none_phase_outputs_returns_error(self) -> None:
        tool = make_query_phase_context_tool(phase_outputs=None)
        result = await tool.execute(tool.input_model(phase="any"), _ctx())
        assert result.is_error


# ---------------------------------------------------------------------------
# query_phase_context — full phase output (no key)
# ---------------------------------------------------------------------------


class TestQueryPhaseContextFullOutput:
    async def test_returns_full_phase_output_when_no_key(self) -> None:
        outputs = {"p1": {"result": "done", "count": 42}}
        tool = make_query_phase_context_tool(phase_outputs=outputs)
        result = await tool.execute(tool.input_model(phase="p1"), _ctx())
        assert not result.is_error
        data = json.loads(result.output)
        assert data["result"] == "done"
        assert data["count"] == 42

    async def test_returns_exactly_the_stored_dict(self) -> None:
        outputs = {"analysis": {"items": [1, 2, 3], "summary": "ok"}}
        tool = make_query_phase_context_tool(phase_outputs=outputs)
        result = await tool.execute(tool.input_model(phase="analysis"), _ctx())
        assert not result.is_error
        data = json.loads(result.output)
        assert data["items"] == [1, 2, 3]

    async def test_multiple_phases_each_accessible(self) -> None:
        outputs = {
            "phase_a": {"x": 1},
            "phase_b": {"y": 2},
        }
        tool = make_query_phase_context_tool(phase_outputs=outputs)
        ra = await tool.execute(tool.input_model(phase="phase_a"), _ctx())
        rb = await tool.execute(tool.input_model(phase="phase_b"), _ctx())
        assert not ra.is_error and not rb.is_error
        assert json.loads(ra.output)["x"] == 1
        assert json.loads(rb.output)["y"] == 2


# ---------------------------------------------------------------------------
# query_phase_context — specific key lookup
# ---------------------------------------------------------------------------


class TestQueryPhaseContextKeyLookup:
    async def test_returns_specific_key_value(self) -> None:
        outputs = {"p1": {"alpha": "hello", "beta": "world"}}
        tool = make_query_phase_context_tool(phase_outputs=outputs)
        result = await tool.execute(tool.input_model(phase="p1", key="alpha"), _ctx())
        assert not result.is_error
        data = json.loads(result.output)
        assert data["alpha"] == "hello"

    async def test_missing_key_returns_error(self) -> None:
        outputs = {"p1": {"alpha": "hello"}}
        tool = make_query_phase_context_tool(phase_outputs=outputs)
        result = await tool.execute(tool.input_model(phase="p1", key="missing"), _ctx())
        assert result.is_error
        payload = json.loads(result.output)
        assert "error" in payload

    async def test_missing_key_error_lists_available_keys(self) -> None:
        outputs = {"p1": {"alpha": "hello", "beta": "world"}}
        tool = make_query_phase_context_tool(phase_outputs=outputs)
        result = await tool.execute(tool.input_model(phase="p1", key="gone"), _ctx())
        payload = json.loads(result.output)
        available = payload.get("available_keys", [])
        assert "alpha" in available
        assert "beta" in available

    async def test_key_with_complex_value_serialized(self) -> None:
        outputs = {"p1": {"nested": {"a": 1, "b": [2, 3]}}}
        tool = make_query_phase_context_tool(phase_outputs=outputs)
        result = await tool.execute(tool.input_model(phase="p1", key="nested"), _ctx())
        assert not result.is_error
        data = json.loads(result.output)
        assert data["nested"]["b"] == [2, 3]


# ---------------------------------------------------------------------------
# query_phase_context — tool metadata
# ---------------------------------------------------------------------------


class TestQueryPhaseContextToolMetadata:
    def test_tool_name(self) -> None:
        tool = make_query_phase_context_tool()
        assert tool.name == "query_phase_context"

    def test_description_is_set(self) -> None:
        tool = make_query_phase_context_tool()
        assert tool.description

    def test_input_model_has_phase_field(self) -> None:
        tool = make_query_phase_context_tool()
        schema = tool.input_model.model_json_schema()
        assert "phase" in schema.get("properties", {})

    def test_input_model_has_key_field(self) -> None:
        tool = make_query_phase_context_tool()
        schema = tool.input_model.model_json_schema()
        assert "key" in schema.get("properties", {})


# ---------------------------------------------------------------------------
# list_phases — empty
# ---------------------------------------------------------------------------


class TestListPhasesEmpty:
    async def test_empty_outputs_returns_empty_phases_list(self) -> None:
        tool = make_list_phases_tool(phase_outputs={})
        result = await tool.execute(tool.input_model(), _ctx())
        assert not result.is_error
        data = json.loads(result.output)
        assert data["phases"] == []

    async def test_empty_outputs_includes_note(self) -> None:
        tool = make_list_phases_tool(phase_outputs={})
        result = await tool.execute(tool.input_model(), _ctx())
        data = json.loads(result.output)
        assert "note" in data

    async def test_none_phase_outputs_treated_as_empty(self) -> None:
        tool = make_list_phases_tool(phase_outputs=None)
        result = await tool.execute(tool.input_model(), _ctx())
        assert not result.is_error
        data = json.loads(result.output)
        assert data["phases"] == []


# ---------------------------------------------------------------------------
# list_phases — with data
# ---------------------------------------------------------------------------


class TestListPhasesWithData:
    async def test_lists_phase_names(self) -> None:
        outputs = {
            "discovery": {"findings": []},
            "analysis": {"score": 0.9},
        }
        tool = make_list_phases_tool(phase_outputs=outputs)
        result = await tool.execute(tool.input_model(), _ctx())
        assert not result.is_error
        data = json.loads(result.output)
        phase_names = {p["phase"] for p in data["phases"]}
        assert phase_names == {"discovery", "analysis"}

    async def test_lists_output_keys_per_phase(self) -> None:
        outputs = {
            "discovery": {"findings": [], "count": 0},
        }
        tool = make_list_phases_tool(phase_outputs=outputs)
        result = await tool.execute(tool.input_model(), _ctx())
        data = json.loads(result.output)
        phase_entry = data["phases"][0]
        assert set(phase_entry["keys"]) == {"findings", "count"}

    async def test_multiple_phases_all_present(self) -> None:
        outputs = {f"phase_{i}": {"k": i} for i in range(5)}
        tool = make_list_phases_tool(phase_outputs=outputs)
        result = await tool.execute(tool.input_model(), _ctx())
        data = json.loads(result.output)
        assert len(data["phases"]) == 5

    async def test_result_has_phases_key(self) -> None:
        tool = make_list_phases_tool(phase_outputs={"p": {"x": 1}})
        result = await tool.execute(tool.input_model(), _ctx())
        data = json.loads(result.output)
        assert "phases" in data


# ---------------------------------------------------------------------------
# list_phases — tool metadata
# ---------------------------------------------------------------------------


class TestListPhasesToolMetadata:
    def test_tool_name(self) -> None:
        tool = make_list_phases_tool()
        assert tool.name == "list_phases"

    def test_description_is_set(self) -> None:
        tool = make_list_phases_tool()
        assert tool.description
