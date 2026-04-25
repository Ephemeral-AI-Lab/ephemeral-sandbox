# ruff: noqa
"""Live E2E: test-anthropic-native-agent — MiniMax via Anthropic-native client.

Tests the complete pipeline using the new AnthropicClient:
- Model registration with api_format="anthropic"
- Config-backed agent definition API boundaries
- Tool invocation through Anthropic-native streaming
- Mid-stream tool event ordering

Run with: pytest tests/test_e2e/test_anthropic_native_agent.py -m live -v
"""

from __future__ import annotations

import uuid

import pytest

from engine.testing.eval_agent import EvalAgent
from tests.test_e2e.conftest import (
    HAS_DAYTONA,
    create_eval_agent,
    create_test_sandbox,
    delete_test_sandbox,
    make_live_client,
)

pytestmark = [pytest.mark.e2e, pytest.mark.live]

# ---------------------------------------------------------------------------
# MiniMax Anthropic-compatible credentials
# ---------------------------------------------------------------------------

MINIMAX_ANTHROPIC_KEY = "sk-cp-Ril2d0sHwI7gagi0S5s9XWFvfPpe6Y8Ms0N7FxpILv93jZCXJDmEiWGRjVALI4VKvSr2XhJfYs5_wLYfhB4QPKWKd4IJHkfZBLhRXQR5tAnjwKiItvcYg-o"
MINIMAX_ANTHROPIC_MODEL = "MiniMax-M2.7"
MINIMAX_ANTHROPIC_BASE_URL = "https://api.minimax.io/anthropic"
MINIMAX_ANTHROPIC_FORMAT = "anthropic"

MODEL_KEY = "minimax-anthropic-native"
AGENT_PROMPT = (
    "You are test-anthropic-native-agent, a developer with a remote Daytona sandbox. "
    "You MUST use tools for every action — never just describe what you'd do. "
    "Use daytona_write_file to create files, daytona_shell to run commands, "
    "daytona_read_file to read files. "
    "Always execute every step using tools. Be concise."
)


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _register_model(client) -> dict:
    """Register the MiniMax model with Anthropic format via API."""
    resp = client.post(
        "/api/db/models/register",
        json={
            "key": MODEL_KEY,
            "label": "MiniMax 2.7 (Anthropic-native)",
            "class_path": "providers.clients.anthropic_native.AnthropicClient",
            "kwargs": {
                "api_key": MINIMAX_ANTHROPIC_KEY,
                "base_url": MINIMAX_ANTHROPIC_BASE_URL,
                "model": MINIMAX_ANTHROPIC_MODEL,
                "api_format": MINIMAX_ANTHROPIC_FORMAT,
            },
            "activate": True,
        },
    )
    assert resp.status_code == 200, f"Model registration failed: {resp.status_code} {resp.text}"
    return resp.json()

# ===========================================================================
# Test: Model Registration and Read-only Agent API
# ===========================================================================


class TestAnthropicNativeModelSetup:
    """Tests model registration with read-only agent definition endpoints."""

    @pytest.fixture()
    def client(self, db_session_factory, tmp_path, monkeypatch):
        c = make_live_client(
            db_session_factory,
            tmp_path,
            monkeypatch,
            api_key=MINIMAX_ANTHROPIC_KEY,
            model=MINIMAX_ANTHROPIC_MODEL,
            base_url=MINIMAX_ANTHROPIC_BASE_URL,
        )
        with c:
            yield c

    def test_model_registered_with_anthropic_format(self, client):
        result = _register_model(client)
        assert result["ok"] is True

        # Verify model is listed
        resp = client.get("/api/db/models")
        assert resp.status_code == 200
        data = resp.json()
        models = data.get("models", [])
        keys = [m["key"] for m in models]
        assert MODEL_KEY in keys

    def test_model_is_active(self, client):
        _register_model(client)
        resp = client.get("/api/db/models/active")
        assert resp.status_code == 200
        active = resp.json()
        assert active["key"] == MODEL_KEY

    def test_agent_definition_api_rejects_model_specific_creation(self, client):
        _register_model(client)
        resp = client.post(
            "/api/agents/",
            json={
                "name": "test-anthropic-native-agent",
                "description": "E2E test Anthropic-native agent",
                "model": MODEL_KEY,
                "tools": ["daytona_shell"],
                "system_prompt": AGENT_PROMPT,
            },
        )

        assert resp.status_code == 405
        assert "file-backed under backend/config/agents" in resp.json()["detail"]


# ===========================================================================
# Test: Basic Agent Communication (migrated to EvalAgent)
# ===========================================================================


@pytest.fixture(scope="module")
def sandbox_id():
    if not EvalAgent.has_all():
        pytest.skip("LLM + Daytona credentials required")
    sb = create_test_sandbox("anthropic-native")
    yield sb["id"]
    delete_test_sandbox(sb["id"])


@pytest.mark.asyncio
async def test_agent_responds_to_simple_prompt(sandbox_id):
    agent = create_eval_agent(sandbox_id=sandbox_id, system_prompt=AGENT_PROMPT, tool_call_limit=5)
    result = await agent.invoke("Say hello in exactly 3 words.")
    assert len(result.assistant_turns()) > 0, "Missing assistant response"
    assert result.text, "Should produce a response"


@pytest.mark.asyncio
async def test_agent_uses_daytona_shell_tool(sandbox_id):
    agent = create_eval_agent(sandbox_id=sandbox_id, system_prompt=AGENT_PROMPT, tool_call_limit=10)
    result = await agent.invoke("Run this exact command in the sandbox: echo 'ANTHROPIC_BASH_OK'")
    assert len(result.assistant_turns()) > 0, "Missing assistant response"

    tool_started = result.tools_started()
    tool_names = [ev.tool_name for ev in tool_started]
    assert any("daytona" in t for t in tool_names), f"No daytona tool used: {tool_names}"


@pytest.mark.asyncio
async def test_agent_multi_tool_interaction(sandbox_id):
    agent = create_eval_agent(sandbox_id=sandbox_id, system_prompt=AGENT_PROMPT, tool_call_limit=10)
    result = await agent.invoke("Use daytona_shell to run: pwd")
    assert len(result.assistant_turns()) > 0, "Missing assistant response"

    tool_started = result.tools_started()
    tool_completed = result.tools_completed()
    assert len(tool_started) >= 1, f"Should use at least one tool. Tools: {[ev.tool_name for ev in tool_started]}"
    assert len(tool_completed) >= 1, f"Should have tool completion. Tools: {[ev.tool_name for ev in tool_completed]}"


@pytest.mark.asyncio
async def test_agent_multi_step_pipeline(sandbox_id):
    agent = create_eval_agent(sandbox_id=sandbox_id, system_prompt=AGENT_PROMPT, tool_call_limit=15)
    result = await agent.invoke(
        "Do these steps in the sandbox:\n"
        "1. Use daytona_write_file to create /workspace/anthro_pipeline.py with: print('ANTHRO_PIPELINE_OK')\n"
        "2. Use daytona_shell to run: python3 /workspace/anthro_pipeline.py\n"
        "3. Report the output"
    )
    assert len(result.assistant_turns()) > 0, "Missing assistant response"

    tool_started = result.tools_started()
    if tool_started:
        daytona_tools = [ev for ev in tool_started if "daytona" in ev.tool_name]
        assert len(daytona_tools) >= 1, (
            f"Expected daytona tools. Got: {[ev.tool_name for ev in tool_started]}"
        )

    tool_completed = result.tools_completed()
    all_output = " ".join(ev.output for ev in tool_completed)
    text = result.text

    has_pipeline = "ANTHRO_PIPELINE_OK" in all_output or "ANTHRO_PIPELINE_OK" in text
    assert has_pipeline or len(tool_started) >= 2, (
        f"Should execute pipeline or use multiple tools. "
        f"Output: {all_output[:200]}, Text: {text[:200]}"
    )


# ===========================================================================
# Test: Event Structure (migrated to EvalAgent)
# ===========================================================================


@pytest.mark.asyncio
async def test_tool_started_has_correct_structure(sandbox_id):
    agent = create_eval_agent(sandbox_id=sandbox_id, system_prompt=AGENT_PROMPT, tool_call_limit=10)
    result = await agent.invoke("Use daytona_shell to run: echo 'ANTHRO_STRUCTURE_OK'")
    tool_started = result.tools_started()
    assert len(tool_started) >= 1, f"No tool_started events."

    for ev in tool_started:
        assert ev.tool_input is not None, f"tool_started missing tool_input: {ev}"
        assert isinstance(ev.tool_input, dict), f"tool_input should be dict: {type(ev.tool_input)}"


@pytest.mark.asyncio
async def test_event_lifecycle_complete(sandbox_id):
    agent = create_eval_agent(sandbox_id=sandbox_id, system_prompt=AGENT_PROMPT, tool_call_limit=10)
    result = await agent.invoke("Use daytona_shell to run: echo 'LIFECYCLE_OK'")

    assert len(result.assistant_turns()) > 0, "Missing assistant response"
    tool_started = result.tools_started()
    tool_completed = result.tools_completed()
    if len(tool_started) > 0 and not result.has_errors:
        assert len(tool_completed) > 0, "tool_started without tool_completed"
