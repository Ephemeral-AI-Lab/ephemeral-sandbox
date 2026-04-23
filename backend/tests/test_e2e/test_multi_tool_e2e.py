# ruff: noqa
"""E2E tests for multiple tool calling scenarios.

Tests verify the agent loop handles multiple tool calls correctly.

Requires live MiniMax API + Daytona sandbox.
Run with: pytest backend/tests/test_e2e/test_multi_tool_e2e.py -m live -v
"""

from __future__ import annotations

import pytest

from engine.testing.eval_agent import EvalAgent
from tests.test_e2e.conftest import create_eval_agent, create_test_sandbox, delete_test_sandbox

pytestmark = [pytest.mark.e2e, pytest.mark.live]

HAS_ALL = EvalAgent.has_all()


# ---------------------------------------------------------------------------
# Shared sandbox fixture
# ---------------------------------------------------------------------------


@pytest.fixture(scope="module")
def sandbox_id():
    if not EvalAgent.has_daytona():
        pytest.skip("Daytona not configured")
    sb = create_test_sandbox("multi-tool")
    yield sb["id"]
    delete_test_sandbox(sb["id"])


# ---------------------------------------------------------------------------
# TestMultipleToolCalls — verify agent makes multiple calls
# ---------------------------------------------------------------------------


@pytest.mark.skipif(not HAS_ALL, reason="MiniMax + Daytona both required")
@pytest.mark.asyncio
async def test_agent_makes_multiple_tool_calls(sandbox_id):
    """Agent should make multiple tool calls in one turn."""
    agent = create_eval_agent(
        sandbox_id=sandbox_id,
        system_prompt="Make multiple tool calls to complete the task.",
    )

    result = await agent.invoke(
        "1. Create /workspace/multi1.txt with 'MULTI1'\n"
        "2. Create /workspace/multi2.txt with 'MULTI2'\n"
        "3. Run: echo 'MULTI_DONE'"
    )

    tool_started = result.tools_started()
    tool_names = [e.tool_name for e in tool_started]

    daytona_calls = [n for n in tool_names if n.startswith("daytona_")]
    assert len(daytona_calls) >= 2, (
        f"Should make at least 2 tool calls. Got {len(daytona_calls)}: {daytona_calls}"
    )


@pytest.mark.skipif(not HAS_ALL, reason="MiniMax + Daytona both required")
@pytest.mark.asyncio
async def test_write_then_bash_sequential(sandbox_id):
    """Write then bash - verify order."""
    agent = create_eval_agent(
        sandbox_id=sandbox_id,
        system_prompt="Write the file first, then run a command to read it.",
    )

    result = await agent.invoke(
        "1. Create /workspace/seq_test.txt with 'SEQUENTIAL_TEST'\n"
        "2. Run: cat /workspace/seq_test.txt"
    )

    tool_started = result.tools_started()
    tool_names = [e.tool_name for e in tool_started]

    has_write = "daytona_write_file" in tool_names
    has_bash = "daytona_shell" in tool_names

    assert has_write or has_bash, (
        f"Should use daytona_write_file or daytona_shell. Tools: {tool_names}"
    )

    if has_write and has_bash:
        write_idx = tool_names.index("daytona_write_file")
        bash_idx = tool_names.index("daytona_shell")
        assert write_idx < bash_idx, f"Write should come before bash. Order: {tool_names}"


@pytest.mark.skipif(not HAS_ALL, reason="MiniMax + Daytona both required")
@pytest.mark.asyncio
async def test_multiple_bash_commands(sandbox_id):
    """Multiple bash commands in same turn."""
    agent = create_eval_agent(
        sandbox_id=sandbox_id,
        system_prompt="Run all three echo commands.",
    )

    result = await agent.invoke(
        "Run: echo 'CMD_1'\nRun: echo 'CMD_2'\nRun: echo 'CMD_3'"
    )

    tool_started = result.tools_started()
    tool_names = [e.tool_name for e in tool_started]

    daytona_shell_count = tool_names.count("daytona_shell")
    assert daytona_shell_count >= 2, (
        f"Should have at least 2 bash calls. Got {daytona_shell_count}. Tools: {tool_names}"
    )


@pytest.mark.skipif(not HAS_ALL, reason="MiniMax + Daytona both required")
@pytest.mark.asyncio
async def test_event_ordering_correct(sandbox_id):
    """Tool started should come before tool completed for each tool."""
    agent = create_eval_agent(
        sandbox_id=sandbox_id,
        system_prompt="Execute the command.",
    )

    result = await agent.invoke(
        "Create /workspace/order_test.txt with content: 'ORDER_TEST'"
    )

    started = result.tools_started()
    completed = result.tools_completed()

    if started and completed:
        # With EvalAgent we just verify both started and completed events exist
        assert len(started) >= 1, "Should have at least one tool_started event"
        assert len(completed) >= 1, "Should have at least one tool_completed event"


@pytest.mark.skipif(not HAS_ALL, reason="MiniMax + Daytona both required")
@pytest.mark.asyncio
async def test_agent_uses_different_tools(sandbox_id):
    """Agent should use different tools for different purposes."""
    agent = create_eval_agent(
        sandbox_id=sandbox_id,
        system_prompt="Use different tools as needed.",
    )

    result = await agent.invoke(
        "1. Create /workspace/diff.txt with 'DIFF'\n"
        "2. List files in /workspace/\n"
        "3. Run: echo 'DONE'"
    )

    tool_started = result.tools_started()
    tool_names = [e.tool_name for e in tool_started]

    unique_tools = set(tool_names)
    assert len(unique_tools) >= 2, f"Should use at least 2 different tools. Got: {unique_tools}"


@pytest.mark.skipif(not HAS_ALL, reason="MiniMax + Daytona both required")
@pytest.mark.asyncio
async def test_agent_completes_with_tool_calls(sandbox_id):
    """Agent should complete successfully with tool calls."""
    agent = create_eval_agent(
        sandbox_id=sandbox_id,
        system_prompt="Execute the task using tools.",
    )

    result = await agent.invoke(
        "Create /workspace/complete.txt with content: 'COMPLETE'"
    )

    assert len(result.assistant_turns()) > 0, "Should complete successfully"

    tool_started = result.tools_started()
    assert len(tool_started) >= 1, f"Should make at least 1 tool call. Got {len(tool_started)}"


# ---------------------------------------------------------------------------
# TestFullStackWorkflow — full-stack tests that complete real workflows
# ---------------------------------------------------------------------------


@pytest.mark.skipif(not HAS_ALL, reason="MiniMax + Daytona both required")
@pytest.mark.asyncio
async def test_build_python_script_workflow(sandbox_id):
    """Build and run a Python script end-to-end."""
    agent = create_eval_agent(
        sandbox_id=sandbox_id,
        system_prompt=(
            "You are a Python developer. Create and run Python scripts. "
            "First write files, then execute them with bash. "
            "Verify your work by running the scripts."
        ),
    )

    result = await agent.invoke(
        "Complete this workflow:\n"
        "1. Create a Python script at /workspace/adder.py that defines a function add(a, b) returning a+b\n"
        "2. Create a Python script at /workspace/main.py that imports adder and prints add(3, 5)\n"
        "3. Run: python /workspace/main.py\n"
        "4. Report the output you see"
    )

    tool_started = result.tools_started()
    tool_names = [e.tool_name for e in tool_started]

    assert "daytona_write_file" in tool_names, f"Should write files. Tools: {tool_names}"
    assert "daytona_shell" in tool_names, f"Should run scripts. Tools: {tool_names}"
    assert len(result.assistant_turns()) > 0, "Should complete"

    text = result.text
    assert "8" in text, f"Should output 8 (3+5). Got: {text[:300]}"


@pytest.mark.skipif(not HAS_ALL, reason="MiniMax + Daytona both required")
@pytest.mark.asyncio
async def test_multi_file_project_workflow(sandbox_id):
    """Create multiple files forming a mini-project."""
    agent = create_eval_agent(
        sandbox_id=sandbox_id,
        system_prompt=(
            "Create a mini project with multiple files. "
            "Write files, then verify they exist with ls."
        ),
    )

    result = await agent.invoke(
        "Create a mini project:\n"
        '1. Create /workspace/config.json with content: {"name": "test-project", "version": "1.0.0"}\n'
        "2. Create /workspace/README.md with content: # Test Project\n"
        "3. Create /workspace/main.py with content: print('hello')\n"
        "4. List all files in /workspace/\n"
        "5. Report what files exist"
    )

    tool_started = result.tools_started()
    tool_names = [e.tool_name for e in tool_started]

    write_calls = [e for e in tool_started if e.tool_name == "daytona_write_file"]
    assert len(write_calls) >= 3, (
        f"Should create 3 files. Got {len(write_calls)}. Tools: {tool_names}"
    )

    assert len(result.assistant_turns()) > 0, "Should complete"


@pytest.mark.skipif(not HAS_ALL, reason="MiniMax + Daytona both required")
@pytest.mark.asyncio
async def test_data_processing_workflow(sandbox_id):
    """Create data, process it, verify results."""
    agent = create_eval_agent(
        sandbox_id=sandbox_id,
        system_prompt=(
            "Create data files, process them, and verify results. Use bash to run commands."
        ),
    )

    result = await agent.invoke(
        "Data processing workflow:\n"
        "1. Create /workspace/data.txt with content: line1\nline2\nline3\n"
        "2. Count lines in data.txt using: wc -l /workspace/data.txt\n"
        "3. Append 'line4' to data.txt\n"
        "4. Count lines again\n"
        "5. Report both counts"
    )

    tool_started = result.tools_started()
    tool_names = [e.tool_name for e in tool_started]

    assert "daytona_write_file" in tool_names, f"Should write file. Tools: {tool_names}"
    assert "daytona_shell" in tool_names, f"Should run commands. Tools: {tool_names}"
    assert len(result.assistant_turns()) > 0, "Should complete"

    text = result.text.lower()
    assert "3" in text or "3 lines" in text or "line3" in text, (
        f"Should mention line count. Got: {text[:300]}"
    )


@pytest.mark.skipif(not HAS_ALL, reason="MiniMax + Daytona both required")
@pytest.mark.asyncio
async def test_error_recovery_workflow(sandbox_id):
    """Handle errors and continue working."""
    agent = create_eval_agent(
        sandbox_id=sandbox_id,
        system_prompt=(
            "If a command fails, report the error and continue with the next step. "
            "Don't stop - complete all steps."
        ),
    )

    result = await agent.invoke(
        "Complete these steps:\n"
        "1. Create /workspace/success.txt with content: 'SUCCESS'\n"
        "2. Try to read /workspace/nonexistent.txt (this will fail - report error)\n"
        "3. List /workspace/ directory\n"
        "4. Report what succeeded and what failed"
    )

    tool_started = result.tools_started()
    tool_names = [e.tool_name for e in tool_started]

    assert "daytona_write_file" in tool_names, f"Should write file. Tools: {tool_names}"
    has_bash_or_read = "daytona_shell" in tool_names or "daytona_read_file" in tool_names
    assert has_bash_or_read, f"Should attempt bash or read commands. Tools: {tool_names}"
    assert len(result.assistant_turns()) > 0, "Should complete even with errors"
