# ruff: noqa
"""Live E2E: test-minimax-agent — real MiniMax LLM + real Daytona sandbox.

This agent is equipped with the MiniMax model and uses Daytona sandbox
for all tool execution. Tests the complete pipeline:
- Sandbox attachment
- Tool invocation (bash, read, write, glob, grep)
- Result verification

Run with: pytest tests/test_e2e/test_minimax_agent.py -m live -v
"""

from __future__ import annotations

import uuid

import pytest

from engine.testing.eval_agent import EvalAgent
from tests.test_e2e.conftest import create_eval_agent, create_test_sandbox, delete_test_sandbox

pytestmark = [pytest.mark.e2e, pytest.mark.live]


# ---------------------------------------------------------------------------
# Agent config
# ---------------------------------------------------------------------------

MINIMAX_AGENT_PROMPT = (
    "You are test-minimax-agent, a developer with a remote Daytona sandbox. "
    "You MUST use tools for every action — never just describe what you'd do. "
    "Use daytona_write_file to create files, daytona_shell to run commands, "
    "daytona_read_file to read files, "
    "daytona_grep to search content, daytona_glob to find files. "
    "Always execute every step using tools. Be concise."
)


# ===========================================================================
# Fixtures
# ===========================================================================


@pytest.fixture(scope="module")
def sandbox_id():
    if not EvalAgent.has_all():
        pytest.skip("LLM + Daytona credentials required")
    sb = create_test_sandbox("minimax-basic")
    yield sb["id"]
    delete_test_sandbox(sb["id"])


@pytest.fixture(scope="module")
def sandbox_id_events():
    if not EvalAgent.has_all():
        pytest.skip("LLM + Daytona credentials required")
    sb = create_test_sandbox("minimax-events")
    yield sb["id"]
    delete_test_sandbox(sb["id"])


# ===========================================================================
# Basic Tests
# ===========================================================================


@pytest.mark.asyncio
async def test_agent_responds_to_simple_prompt(sandbox_id):
    agent = create_eval_agent(sandbox_id=sandbox_id, system_prompt=MINIMAX_AGENT_PROMPT)
    result = await agent.invoke("Say hello in exactly 3 words.")

    assert len(result.assistant_turns()) > 0, "Missing assistant_complete turn"
    assert result.text, "Should produce a response"


@pytest.mark.asyncio
async def test_agent_uses_daytona_shell_tool(sandbox_id):
    agent = create_eval_agent(sandbox_id=sandbox_id, system_prompt=MINIMAX_AGENT_PROMPT)
    result = await agent.invoke("Run this exact command in the sandbox: echo 'MINIMAX_BASH_OK'")

    assert len(result.assistant_turns()) > 0, "Missing assistant_complete turn"

    tool_started = result.tools_started()
    tool_names = [ev.tool_name for ev in tool_started]
    assert any("daytona" in t for t in tool_names), f"No daytona tool used: {tool_names}"


@pytest.mark.asyncio
async def test_agent_write_and_read_file(sandbox_id):
    agent = create_eval_agent(sandbox_id=sandbox_id, system_prompt=MINIMAX_AGENT_PROMPT)
    marker = f"MINIMAX_READBACK_{uuid.uuid4().hex[:8]}"
    result = await agent.invoke(
        f"Write '{marker}' to /workspace/minimax_test.txt using daytona_write_file, "
        f"then read it back using daytona_shell: cat /workspace/minimax_test.txt"
    )

    assert len(result.assistant_turns()) > 0

    tool_completed = result.tools_completed()
    all_output = " ".join(ev.output for ev in tool_completed)
    text = result.text

    has_marker = marker in all_output or marker in text
    assert has_marker, (
        f"Should find marker '{marker}' in output. Output: {all_output[:200]}, Text: {text[:200]}"
    )


@pytest.mark.asyncio
async def test_agent_lists_files(sandbox_id):
    agent = create_eval_agent(sandbox_id=sandbox_id, system_prompt=MINIMAX_AGENT_PROMPT)
    result = await agent.invoke("Use daytona_shell to run 'ls /workspace'")

    assert len(result.assistant_turns()) > 0

    tool_started = result.tools_started()
    tool_names = [ev.tool_name for ev in tool_started]
    assert any("daytona_shell" in t for t in tool_names), (
        f"Expected listing via daytona_shell. Got: {tool_names}"
    )


@pytest.mark.asyncio
async def test_agent_grep_search(sandbox_id):
    agent = create_eval_agent(sandbox_id=sandbox_id, system_prompt=MINIMAX_AGENT_PROMPT)

    # Setup: create the file to search
    await agent.invoke(
        "Use daytona_shell to run: echo 'GREP_TARGET_MINIMAX' > /workspace/searchable.txt"
    )

    # Search for the content
    agent2 = create_eval_agent(sandbox_id=sandbox_id, system_prompt=MINIMAX_AGENT_PROMPT)
    result = await agent2.invoke(
        "Use daytona_grep to search for 'GREP_TARGET' in /workspace/"
    )

    assert len(result.assistant_turns()) > 0

    tool_started = result.tools_started()
    tool_names = [ev.tool_name for ev in tool_started]
    assert any("daytona_grep" in t or "daytona_shell" in t for t in tool_names), (
        f"Expected grep or bash tool. Got: {tool_names}"
    )


@pytest.mark.asyncio
async def test_agent_glob_find(sandbox_id):
    agent = create_eval_agent(sandbox_id=sandbox_id, system_prompt=MINIMAX_AGENT_PROMPT)

    # Setup: create files to glob
    await agent.invoke(
        "Use daytona_shell to run: touch /workspace/glob_test_1.txt /workspace/glob_test_2.txt"
    )

    # Glob for .txt files
    agent2 = create_eval_agent(sandbox_id=sandbox_id, system_prompt=MINIMAX_AGENT_PROMPT)
    result = await agent2.invoke(
        "Use daytona_glob to find all .txt files in /workspace/"
    )

    assert len(result.assistant_turns()) > 0


@pytest.mark.asyncio
async def test_agent_multi_step_pipeline(sandbox_id):
    agent = create_eval_agent(sandbox_id=sandbox_id, system_prompt=MINIMAX_AGENT_PROMPT)
    result = await agent.invoke(
        "Do these steps in the sandbox:\n"
        "1. Use daytona_write_file to create /workspace/pipeline.py with: print('PIPELINE_OK')\n"
        "2. Use daytona_shell to run: python3 /workspace/pipeline.py\n"
        "3. Report the output"
    )

    assert len(result.assistant_turns()) > 0

    tool_started = result.tools_started()
    daytona_tools = [ev for ev in tool_started if "daytona" in ev.tool_name]
    assert len(daytona_tools) >= 1, (
        f"Expected daytona tools. Got: {[ev.tool_name for ev in tool_started]}"
    )

    tool_completed = result.tools_completed()
    all_output = " ".join(ev.output for ev in tool_completed)
    text = result.text

    has_pipeline = "PIPELINE_OK" in all_output or "PIPELINE_OK" in text
    assert has_pipeline or len(tool_started) >= 2, (
        f"Should execute pipeline or use multiple tools. "
        f"Output: {all_output[:200]}, Text: {text[:200]}"
    )


# ===========================================================================
# Event Structure Tests
# ===========================================================================


@pytest.mark.asyncio
async def test_tool_started_has_correct_structure(sandbox_id_events):
    agent = create_eval_agent(sandbox_id=sandbox_id_events, system_prompt=MINIMAX_AGENT_PROMPT)
    result = await agent.invoke("Use daytona_shell to run: echo 'STRUCTURE_OK'")

    tool_started = result.tools_started()
    assert len(tool_started) >= 1, f"No tool_started events. Tool names: {result.tool_names}"

    for ev in tool_started:
        tool_input = ev.tool_input
        assert tool_input is not None, f"tool_started missing tool_input: {ev}"
        assert isinstance(tool_input, dict), f"tool_input should be dict: {type(tool_input)}"

        name = ev.tool_name
        if name == "daytona_shell":
            assert "command" in tool_input, f"daytona_shell missing 'command': {tool_input}"


@pytest.mark.asyncio
async def test_tool_completed_has_output(sandbox_id_events):
    agent = create_eval_agent(sandbox_id=sandbox_id_events, system_prompt=MINIMAX_AGENT_PROMPT)
    result = await agent.invoke("Use daytona_shell to run: echo 'OUTPUT_CHECK'")

    tool_completed = result.tools_completed()
    if tool_completed:
        for ev in tool_completed:
            if not ev.is_error:
                output = ev.output
                assert output, f"Successful tool_completed has empty output: {ev}"


@pytest.mark.asyncio
async def test_event_lifecycle_complete(sandbox_id_events):
    agent = create_eval_agent(sandbox_id=sandbox_id_events, system_prompt=MINIMAX_AGENT_PROMPT)
    result = await agent.invoke("Use daytona_shell to run: echo 'LIFECYCLE_OK'")

    assert len(result.assistant_turns()) > 0, "Missing assistant_complete turn"

    tool_started = result.tools_started()
    tool_completed = result.tools_completed()
    if tool_started and not result.has_errors:
        assert len(tool_completed) > 0, "tool_started without tool_completed"
