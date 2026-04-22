"""Focused tests for caller-aware run_subagent schema narrowing."""

from __future__ import annotations

from pathlib import Path
from types import SimpleNamespace
import sys
import types

import pytest

# The subagent tool import path touches the provider package. Stub the
# Anthropic client surface so this focused schema test can run without the
# optional provider dependency installed.
if "anthropic" not in sys.modules:
    anthropic_stub = types.ModuleType("anthropic")
    anthropic_stub.APIError = type("APIError", (Exception,), {})
    anthropic_stub.APIStatusError = type("APIStatusError", (Exception,), {})
    anthropic_stub.AsyncAnthropic = object
    sys.modules["anthropic"] = anthropic_stub
if "anthropic.types" not in sys.modules:
    sys.modules["anthropic.types"] = types.ModuleType("anthropic.types")

from tools.core.base import ToolExecutionContext
from tools.subagent import SubagentToolkit
from tools.subagent.run_subagent_tool import run_subagent
from team.builtins import register_all as _register_team_builtins


try:
    _register_team_builtins()
except Exception:
    pass


class _StubConfig:
    cwd = Path("/tmp")
    session_id = "session_abc"


def _assert_run_subagent_payload_schema_matches_runtime_xor(
    schema: dict[str, object],
) -> None:
    props = schema["properties"]  # type: ignore[index]
    assert schema["oneOf"] == [{"required": ["prompt"]}, {"required": ["input"]}]

    prompt_schema = props["prompt"]  # type: ignore[index]
    assert prompt_schema["type"] == "string"
    assert prompt_schema["minLength"] == 1
    assert "anyOf" not in prompt_schema
    assert "default" not in prompt_schema

    input_schema = props["input"]  # type: ignore[index]
    assert input_schema["type"] == "object"
    assert "anyOf" not in input_schema
    assert "default" not in input_schema


@pytest.mark.asyncio
async def test_run_subagent_rejects_non_subagent_team_role_targets():
    ctx = ToolExecutionContext(
        cwd=Path("/tmp"),
        metadata={"session_config": _StubConfig()},
    )

    result = await run_subagent.execute(
        run_subagent.input_model(agent_name="developer", prompt="serialize"),
        ctx,
    )

    assert result.is_error is True
    assert "developer" in result.output
    assert "is not a subagent" in result.output


def test_run_subagent_api_schema_requires_one_of_prompt_or_input():
    schema = run_subagent.to_api_schema()["input_schema"]

    _assert_run_subagent_payload_schema_matches_runtime_xor(schema)


def test_subagent_toolkit_schema_limits_planner_to_scout():
    toolkit = SubagentToolkit.from_context(
        SimpleNamespace(metadata={"agent_name": "team_planner"})
    )

    schema = toolkit.list_tools()[0].to_api_schema()["input_schema"]
    enum = schema["properties"]["agent_name"]["enum"]

    assert enum == ["scout"]
    _assert_run_subagent_payload_schema_matches_runtime_xor(schema)


def test_subagent_toolkit_schema_limits_replanner_to_scout():
    toolkit = SubagentToolkit.from_context(
        SimpleNamespace(metadata={"agent_name": "team_replanner"})
    )

    schema = toolkit.list_tools()[0].to_api_schema()["input_schema"]
    enum = schema["properties"]["agent_name"]["enum"]

    assert enum == ["scout"]


def test_subagent_toolkit_schema_excludes_non_subagent_team_roles():
    toolkit = SubagentToolkit.from_context(
        SimpleNamespace(metadata={"agent_name": "coordinator"})
    )

    schema = toolkit.list_tools()[0].to_api_schema()["input_schema"]
    enum = schema["properties"]["agent_name"]["enum"]

    assert "scout" in enum
    assert "developer" not in enum
    assert "validator" not in enum
    assert "team_planner" not in enum
