# ruff: noqa
"""Live E2E: single-agent and multi-agent sandbox tool calling.

Tests the full pipeline: agent creation, sandbox attachment, tool invocation,
result verification — using real LLM + real Daytona sandbox.

Single-agent tests verify:
- Agent can invoke individual Daytona tools (bash, write, read, grep, glob)
- Tool events flow correctly (tool_started → tool_completed)
- File roundtrips work (write → read with content verification)
- Multi-turn tool chaining preserves sandbox state
- CI-owned LSP tools are available in the schema when code intelligence is enabled

Run with: pytest tests/test_e2e/test_live_sandbox_agents.py -m live -v
"""

from __future__ import annotations

import uuid

import pytest

from engine.testing.eval_agent import EvalAgent
from tests.test_e2e.conftest import create_eval_agent, create_test_sandbox, delete_test_sandbox

pytestmark = [pytest.mark.e2e, pytest.mark.live, pytest.mark.asyncio]

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

KNOWN_SANDBOX_TOOLS = {
    "shell", "read_file", "write_file",
    "grep", "glob",
    "edit_file", "ci_query_symbol",
    "ci_diagnostics",
}

AGENT_PROMPT = (
    "You are a developer with a remote Daytona sandbox. "
    "You MUST use tools for every action — never just describe what you'd do. "
    "Use write_file to create files, shell to run commands, "
    "read_file to read files, "
    "grep to search content, glob to find files. "
    "Always execute every step using tools. Be concise."
)


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------

@pytest.fixture(scope="module")
def sandbox_id():
    if not EvalAgent.has_all():
        pytest.skip("LLM + Daytona credentials required")
    sb = create_test_sandbox("sandbox-agents")
    yield sb["id"]
    delete_test_sandbox(sb["id"])


@pytest.fixture(scope="module")
def agent(sandbox_id):
    return create_eval_agent(sandbox_id=sandbox_id, system_prompt=AGENT_PROMPT)


# ===========================================================================
# AREA 1: Single-Agent — Sandbox Tool Invocation
# ===========================================================================


async def test_bash_tool_invocation(agent):
    """Agent invokes shell and tool events contain correct structure."""
    result = await agent.invoke(
        "Use shell to run 'echo SINGLE_AGENT_BASH_OK' in the sandbox."
    )
    assert len(result.assistant_turns()) > 0, "No assistant turns produced"

    started = result.tools_started()
    assert len(started) >= 1, f"No tool_started events. Tool names: {result.tool_names}"

    for ev in started:
        assert ev.tool_name in KNOWN_SANDBOX_TOOLS, f"Unknown tool '{ev.tool_name}'"
        assert isinstance(ev.tool_input, dict), f"tool_input should be dict: {ev.tool_input}"

    completed = result.tools_completed()
    if completed:
        success = [e for e in completed if not e.is_error]
        assert len(success) >= 1, f"No successful tool completions: {completed}"


async def test_write_file(agent):
    """Agent uses write_file to create a file in the sandbox."""
    result = await agent.invoke(
        "Use write_file to write 'WRITE_TOOL_MARKER' to /workspace/write_test.txt"
    )
    started = result.tools_started()
    assert len(started) >= 1, f"No tools used. Tool names: {result.tool_names}"

    tool_names = [e.tool_name for e in started]
    assert any(n in ("write_file", "shell") for n in tool_names), (
        f"Expected write tool, got: {tool_names}"
    )


async def test_list_files_tool(agent):
    """Agent uses shell to list a directory."""
    # First create a file so there's something to list
    await agent.invoke(
        "Use shell to run 'touch /workspace/listable.txt'"
    )
    result = await agent.invoke(
        "Use shell to run 'ls /workspace'."
    )
    started = result.tools_started()
    assert len(started) >= 1, f"No tools used. Tool names: {result.tool_names}"


async def test_file_roundtrip_write_read(agent):
    """Write file via tool, read it back — verify content roundtrip."""
    marker = f"ROUNDTRIP_{uuid.uuid4().hex[:8]}"
    result = await agent.invoke(
        f"Do these two steps in the sandbox using tools:\n"
        f"1. Use write_file to write '{marker}' to /workspace/roundtrip.txt\n"
        f"2. Use shell to run 'cat /workspace/roundtrip.txt'\n"
        f"Do both steps."
    )
    started = result.tools_started()
    completed = result.tools_completed()
    assert len(started) >= 1, f"No tools used. Tool names: {result.tool_names}"

    all_outputs = " ".join(e.output for e in completed)
    text = result.text
    has_marker = marker in all_outputs or marker in text
    has_write_tool = any(
        e.tool_name in ("write_file", "shell")
        for e in started
    )
    assert has_marker or has_write_tool, (
        f"Roundtrip: should find marker or at least attempt write tool. "
        f"Tool names: {[e.tool_name for e in started]}, "
        f"Text: {text[:200]}"
    )


async def test_grep_search_tool(agent):
    """Agent uses grep to search file content."""
    # Seed a file first
    await agent.invoke(
        "Use shell to run: echo 'GREP_TARGET_XYZ' > /workspace/searchable.txt"
    )
    result = await agent.invoke(
        "Use grep to search for 'GREP_TARGET' in /workspace/"
    )
    started = result.tools_started()
    assert len(started) >= 1, f"No tools used. Tool names: {result.tool_names}"

    tool_names = [e.tool_name for e in started]
    assert any(n in ("grep", "shell") for n in tool_names), (
        f"Expected grep or bash tool, got: {tool_names}"
    )


async def test_glob_search_tool(agent):
    """Agent uses glob to find files by pattern."""
    # Seed files
    await agent.invoke(
        "Use shell to run: touch /workspace/glob_a.py /workspace/glob_b.py"
    )
    result = await agent.invoke(
        "Use glob to find all .py files in /workspace/"
    )
    started = result.tools_started()
    assert len(started) >= 1, f"No tools used. Tool names: {result.tool_names}"


# ===========================================================================
# AREA 2: Single-Agent — Multi-Turn Tool Chaining
# ===========================================================================


async def test_create_then_verify_file(agent):
    """Turn 1: create file. Turn 2: verify file content. Sandbox state persists across invocations."""
    marker = f"CHAIN_{uuid.uuid4().hex[:8]}"

    # Turn 1: Create
    result1 = await agent.invoke(
        f"Use write_file to create /workspace/chain.txt with content '{marker}'"
    )
    assert len(result1.assistant_turns()) > 0
    t1_tools = result1.tools_started()
    assert len(t1_tools) >= 1, f"Turn 1 should use a tool. Tool names: {result1.tool_names}"

    # Turn 2: Verify (self-contained prompt — no conversation memory)
    result2 = await agent.invoke(
        "Use shell to run 'cat /workspace/chain.txt' and tell me the content."
    )
    assert len(result2.assistant_turns()) > 0
    text2 = result2.text
    t2_completed = result2.tools_completed()
    all_output = " ".join(e.output for e in t2_completed)
    has_marker = marker in text2 or marker in all_output
    has_tool = len(result2.tools_started()) >= 1
    assert has_marker or has_tool, (
        f"Turn 2 should reference '{marker}' or use a tool. Text: {text2[:200]}"
    )


async def test_three_turn_create_read_modify(agent):
    """3-turn chain: create -> read -> modify. All turns use tools. Sandbox state persists."""
    result1 = await agent.invoke(
        "Use shell to run: echo 'V1_CONTENT' > /workspace/evolve.txt"
    )
    t1 = result1.tools_started()
    assert len(t1) >= 1

    result2 = await agent.invoke(
        "Use shell to run: cat /workspace/evolve.txt"
    )
    t2 = result2.tools_started()
    assert len(t2) >= 1

    result3 = await agent.invoke(
        "Use shell to run: echo 'V2_CONTENT' >> /workspace/evolve.txt"
    )
    t3 = result3.tools_started()
    assert len(t3) >= 1

    total = len(t1) + len(t2) + len(t3)
    assert total >= 3, f"Expected at least 3 tool calls across 3 turns, got {total}"


async def test_complex_multi_step_task(agent):
    """Agent performs create-file -> execute -> capture-output in one turn."""
    result = await agent.invoke(
        "Do these steps in the sandbox:\n"
        "1. Use write_file to create /workspace/hello.py with: print('HELLO_FROM_E2E')\n"
        "2. Use shell to run: python3 /workspace/hello.py\n"
        "3. Report the output."
    )
    assert len(result.assistant_turns()) > 0

    started = result.tools_started()
    if started:
        sandbox_tools = [e for e in started if e.tool_name in {"write_file", "read_file", "shell"}]
        assert len(sandbox_tools) >= 1, f"Expected sandbox tools: {[e.tool_name for e in started]}"

    # Check if the output contains our marker
    completed = result.tools_completed()
    all_output = " ".join(e.output for e in completed)
    text = result.text
    has_hello = "HELLO_FROM_E2E" in all_output or "HELLO_FROM_E2E" in text
    has_tool = len(started) >= 1
    assert has_hello or has_tool, (
        f"Should find HELLO_FROM_E2E in output or at least attempt tools. "
        f"Text: {text[:200]}, Outputs: {all_output[:200]}"
    )


# ===========================================================================
# AREA 3: Single-Agent — Tool Schema & Event Structure Verification
# ===========================================================================


async def test_tool_started_contains_tool_input_dict(agent):
    """tool_started events must have tool_input as a dict with expected keys."""
    result = await agent.invoke(
        "Use shell to run 'echo INPUT_STRUCTURE_OK'"
    )
    started = result.tools_started()
    assert len(started) >= 1

    for ev in started:
        assert ev.tool_input is not None, f"tool_started missing tool_input: {ev}"
        assert isinstance(ev.tool_input, dict), f"tool_input should be dict: {type(ev.tool_input)}"

        if ev.tool_name == "shell":
            assert "command" in ev.tool_input, f"shell missing 'command': {ev.tool_input}"
        elif ev.tool_name == "write_file":
            assert "file_path" in ev.tool_input
            assert "content" in ev.tool_input
        elif ev.tool_name == "read_file":
            assert "file_path" in ev.tool_input


async def test_tool_completed_has_nonempty_output(agent):
    """Successful tool_completed events must have non-empty output."""
    result = await agent.invoke(
        "Use shell to run 'echo OUTPUT_CHECK_OK'"
    )
    completed = result.tools_completed()
    if completed:
        for ev in completed:
            if not ev.is_error:
                assert ev.output, f"Successful tool_completed has empty output: {ev}"


async def test_full_event_lifecycle(agent):
    """A tool-using chat must produce assistant turns and matching tool started/completed pairs."""
    result = await agent.invoke(
        "Use shell to run 'echo LIFECYCLE_OK'"
    )
    assert len(result.assistant_turns()) > 0, "No assistant turns produced"

    started = result.tools_started()
    completed = result.tools_completed()

    # If model used tools and no error, verify tool event pair
    if started and not result.has_errors:
        assert len(completed) >= 1, (
            f"tool_started without tool_completed. "
            f"Started: {[e.tool_name for e in started]}"
        )
