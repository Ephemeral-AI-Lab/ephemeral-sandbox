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

from agents import get_definition as get_agent_definition
from tools.core.base import ToolExecutionContext
from tools.subagent import SubagentToolkit
from tools.subagent.run_subagent_tool import run_subagent
from team.builtins import register_all as _register_team_builtins


if get_agent_definition("submit_plan_agent") is None:
    try:
        _register_team_builtins()
    except Exception:
        pass


class _StubConfig:
    cwd = Path("/tmp")
    session_id = "session_abc"


@pytest.mark.asyncio
async def test_run_subagent_rejects_internal_subagent_targets():
    ctx = ToolExecutionContext(
        cwd=Path("/tmp"),
        metadata={"session_config": _StubConfig()},
    )

    result = await run_subagent.execute(
        run_subagent.input_model(agent_name="submit_plan_agent", prompt="serialize"),
        ctx,
    )

    assert result.is_error is True
    assert "submit_plan_agent" in result.output
    assert result.is_error is True


def test_subagent_toolkit_schema_limits_planner_to_scout():
    toolkit = SubagentToolkit.from_context(
        SimpleNamespace(metadata={"agent_name": "team_planner"})
    )

    schema = toolkit.list_tools()[0].to_api_schema()["input_schema"]
    enum = schema["properties"]["agent_name"]["enum"]

    assert enum == ["scout"]


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
    assert "submit_plan_agent" not in enum
