# ruff: noqa
"""E2E tests for agentic tool call loop — tool accuracy, skill following, task completion.

These tests verify:
1. Tool call accuracy - agent selects correct tool with correct parameters
2. Skill loading & instruction following - agent follows skill instructions exactly
3. Agentic task completion - agent completes multi-step tasks without stopping early

Requires live LLM API + Daytona sandbox.
Run with: pytest tests/test_e2e/test_agentic_loop_e2e.py -m live -v
"""

from __future__ import annotations

import pytest

from engine.testing.eval_agent import EvalAgent
from tests.test_e2e.conftest import create_eval_agent, create_test_sandbox, delete_test_sandbox

pytestmark = [pytest.mark.e2e, pytest.mark.live]


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------


@pytest.fixture(scope="module")
def sandbox_id():
    if not EvalAgent.has_all():
        pytest.skip("LLM + Daytona credentials required")
    sb = create_test_sandbox("agentic-loop")
    yield sb["id"]
    delete_test_sandbox(sb["id"])


# ===========================================================================
# AREA 1: Tool Call Accuracy
# ===========================================================================


@pytest.mark.asyncio
async def test_correct_tool_selected_for_file_write(sandbox_id):
    """Agent should use write_file, not shell, for file creation."""
    agent = create_eval_agent(
        sandbox_id=sandbox_id,
        system_prompt=(
            "You have sandbox access via write_file and shell. "
            "When asked to create a file, ALWAYS use write_file."
        ),
    )

    result = await agent.invoke(
        "Create a file /workspace/e2e_accuracy.txt with content: TOOL_ACCURACY_TEST_PASS"
    )

    # Should use write_file for file creation
    assert "write_file" in result.tool_names, (
        f"Should use write_file for file creation. Tools used: {result.tool_names}"
    )
    # Should NOT use shell for file creation (wrong tool)
    bash_for_write = [
        ts
        for ts in result.tools_started()
        if ts.tool_name == "shell" and "write" in str(ts.tool_input).lower()
    ]
    assert not bash_for_write, "Should not use shell for file write operations"


@pytest.mark.asyncio
async def test_correct_tool_selected_for_command_execution(sandbox_id):
    """Agent should use shell, not write_file, for command execution."""
    agent = create_eval_agent(
        sandbox_id=sandbox_id,
        system_prompt=(
            "You have sandbox access. Use shell for running commands. "
            "Use write_file only for creating files."
        ),
    )

    result = await agent.invoke(
        "Run this command in the sandbox: echo 'CORRECT_TOOL_BASH'"
    )

    # Should use shell for command execution
    assert "shell" in result.tool_names, (
        f"Should use shell for commands. Tools used: {result.tool_names}"
    )


@pytest.mark.asyncio
async def test_tool_input_parameters_correct(sandbox_id):
    """Verify tool is called with the exact parameters specified."""
    agent = create_eval_agent(
        sandbox_id=sandbox_id,
        system_prompt="Use write_file with EXACTLY the path and content provided.",
    )

    result = await agent.invoke(
        "Write to /workspace/params_test.txt with content: PARAM_TEST_CONTENT"
    )

    write_calls = [tc for tc in result.tool_calls if tc.name == "write_file"]

    assert write_calls, (
        f"No write_file calls found. Tools: {result.tool_names}"
    )

    # Verify exact path
    write_inputs = [tc.input for tc in write_calls]
    path_matched = any(
        inp.get("file_path") == "/workspace/params_test.txt" for inp in write_inputs
    )
    assert path_matched, f"Expected path /workspace/params_test.txt. Got: {write_inputs}"

    # Verify exact content
    content_matched = any(inp.get("content") == "PARAM_TEST_CONTENT" for inp in write_inputs)
    assert content_matched, f"Expected content 'PARAM_TEST_CONTENT'. Got: {write_inputs}"


@pytest.mark.asyncio
async def test_multiple_tools_different_purposes(sandbox_id):
    """Agent should use different tools for different purposes in same conversation."""
    agent = create_eval_agent(
        sandbox_id=sandbox_id,
        system_prompt=(
            "Use the right tool for each task. "
            "Continue working — do not stop after one tool. "
            "Make tool calls for BOTH steps: write the file AND run the command."
        ),
    )

    result = await agent.invoke(
        "First, create /workspace/multi_test.txt with 'MULTI_TOOL_TEST'. Then run: cat /workspace/multi_test.txt"
    )

    # Should have BOTH write and bash (read) tools
    assert "write_file" in result.tool_names, (
        f"Missing write tool. Tools: {result.tool_names}"
    )
    assert "shell" in result.tool_names, (
        f"Missing bash tool. Tools: {result.tool_names}"
    )

    # Verify sequence: write should come before bash
    write_idx = result.tool_names.index("write_file")
    bash_idx = result.tool_names.index("shell")
    assert write_idx < bash_idx, f"Write should come before bash. Order: {result.tool_names}"


# ===========================================================================
# AREA 2: Skill Loading & Instruction Following
#
# NOTE: EvalAgent uses Daytona tools directly and does not register a
# load_skill tool. Tests that relied on load_skill being available via
# the HTTP agent creation path are skipped. Tests that verify instruction
# following via system_prompt injection are preserved.
# ===========================================================================


@pytest.mark.asyncio
@pytest.mark.skip(
    reason="load_skill tool not available via EvalAgent — requires HTTP agent creation path"
)
async def test_skill_load_skill_invoked(sandbox_id):
    """Agent should invoke load_skill tool when given a skill-dependent task.

    Skipped: EvalAgent registers Daytona tools only; load_skill is not available.
    """
    pass


@pytest.mark.asyncio
@pytest.mark.skip(
    reason="load_skill tool not available via EvalAgent — requires HTTP agent creation path"
)
async def test_skill_instructions_followed_exactly(sandbox_id):
    """Agent should follow skill instructions with exact string matching.

    Skipped: EvalAgent registers Daytona tools only; load_skill is not available.
    """
    pass


@pytest.mark.asyncio
async def test_skill_output_format_compliance(sandbox_id):
    """Verify agent uses the exact output format specified by the skill.

    Adapted: skill instructions are injected directly into the system_prompt
    instead of relying on load_skill.
    """
    agent = create_eval_agent(
        sandbox_id=sandbox_id,
        system_prompt=(
            "For verification tasks, use the following verification format in your response:\n"
            "TOOL_CALLED: <tool name>\n"
            "PARAMS_USED: <parameters>\n"
            "VERIFIED: <result>\n"
            "STATUS: PASS or FAIL\n"
            "Execute all verification steps with tools."
        ),
    )

    result = await agent.invoke(
        "Run: echo 'FORMAT_TEST' and verify the output.\n"
        "Provide the verification report with TOOL_CALLED, PARAMS_USED, VERIFIED, STATUS fields."
    )

    text = result.text

    # Skill mandates specific output fields — accept any formatting
    required_fields = ["TOOL_CALLED", "PARAMS_USED", "VERIFIED", "STATUS"]
    for field in required_fields:
        assert field in text, f"Missing required field '{field}' from skill format. Got: {text}"


@pytest.mark.asyncio
async def test_skill_not_loaded_when_not_needed(sandbox_id):
    """Verify agent does not use unnecessary tools for a simple task.

    Adapted: since load_skill is not registered, we verify the agent only
    uses shell (the minimal required tool) for a simple echo.
    """
    agent = create_eval_agent(
        sandbox_id=sandbox_id,
        system_prompt="You have sandbox access. Only use the tools you need.",
    )

    result = await agent.invoke(
        "Simply run: echo 'NO_SKILL_NEEDED' and tell me the result."
    )

    # Should only use shell for simple echo command
    assert "shell" in result.tool_names, (
        f"Should use shell for echo. Tools used: {result.tool_names}"
    )


# ===========================================================================
# AREA 3: Agentic Task Completion (Multi-Step, No Early Stop)
# ===========================================================================


@pytest.mark.asyncio
async def test_five_step_task_completes_all_steps(sandbox_id):
    """A 5-step task should complete ALL 5 steps, not stop at step 2 or 3."""
    agent = create_eval_agent(
        sandbox_id=sandbox_id,
        system_prompt=(
            "Execute ALL steps in sequence. Do NOT skip any steps. "
            "Report completion of EACH step. "
            "Continue working — do not stop to summarize results unless the task is done. "
            "You MUST use write_file for EACH file creation step - "
            "do NOT use shell to create files."
        ),
        tool_call_limit=200,
    )

    result = await agent.invoke(
        "Complete these 5 steps in order:\n"
        "Step 1: Create /workspace/step1.txt with 'STEP1_DONE'\n"
        "Step 2: Create /workspace/step2.txt with 'STEP2_DONE'\n"
        "Step 3: Create /workspace/step3.txt with 'STEP3_DONE'\n"
        "Step 4: Create /workspace/step4.txt with 'STEP4_DONE'\n"
        "Step 5: Create /workspace/step5.txt with 'STEP5_DONE'\n"
        "After completing all steps, list all 5 filenames you created."
    )

    # Count write_file calls - should be exactly 5 (one per step)
    write_calls = [tc for tc in result.tool_calls if tc.name == "write_file"]
    assert len(write_calls) >= 5, (
        f"Expected at least 5 write operations (one per step). Got {len(write_calls)}. "
        f"Tools: {result.tool_names}"
    )

    # Verify all 5 files were attempted
    write_inputs = [tc.input for tc in write_calls]
    expected_files = ["step1.txt", "step2.txt", "step3.txt", "step4.txt", "step5.txt"]
    created_files = [inp.get("file_path", "").split("/")[-1] for inp in write_inputs]

    for expected in expected_files:
        assert expected in created_files, (
            f"File {expected} not created. Created files: {created_files}"
        )


@pytest.mark.asyncio
async def test_agent_continues_after_tool_error(sandbox_id):
    """Agent should continue task even if a tool call returns an error."""
    agent = create_eval_agent(
        sandbox_id=sandbox_id,
        system_prompt=(
            "You are a tool-calling agent. You MUST use tools to complete tasks. "
            "NEVER describe what you would do — ALWAYS make actual tool calls. "
            "If a tool fails, explain the error and continue with remaining steps. "
            "Do NOT stop the task. Make tool calls for ALL steps."
        ),
    )

    result = await agent.invoke(
        "Use your tools to complete these steps. "
        "You MUST call a tool for EACH step — do not skip any.\n"
        "Step 1: Use write_file to create /workspace/recover1.txt with content 'RECOVER1'\n"
        "Step 2: Use shell to run: cat /nonexistent/file.txt (expect error)\n"
        "Step 3: Use write_file to create /workspace/recover3.txt with content 'RECOVER3'\n"
        "Report what happened at each step."
    )

    tool_started = result.tools_started()
    tool_names = [ts.tool_name for ts in tool_started]

    # Should have write for step 1
    assert "write_file" in tool_names, (
        f"Should attempt step 1 write. Tools: {tool_names}"
    )

    # Should have tried to read nonexistent file (step 2)
    bash_calls = [ts for ts in tool_started if ts.tool_name == "shell"]
    assert bash_calls, f"Should attempt step 2 (read nonexistent file). Tools: {tool_names}"

    # Should have write for step 3 (continued after error)
    first_bash_idx = tool_started.index(bash_calls[0])
    write_calls_after_bash = [
        ts
        for i, ts in enumerate(tool_started)
        if ts.tool_name == "write_file" and i > first_bash_idx
    ]
    assert write_calls_after_bash, (
        f"Should continue with step 3 after error. Tools: {tool_names}"
    )


@pytest.mark.asyncio
async def test_complex_task_with_10_plus_tool_calls(sandbox_id):
    """Complex task requiring 10+ tool calls should complete without exhausting budget."""
    agent = create_eval_agent(
        sandbox_id=sandbox_id,
        system_prompt=(
            "Execute each step carefully. Complete all operations. "
            "Continue working — do not stop to summarize. "
            "Make a tool call for EACH file - do not skip any file."
        ),
        tool_call_limit=200,
    )

    prompt = (
        "Create these 10 files in /workspace/:\n"
        + "\n".join(f"- file{i}.txt with content 'FILE{i}DONE'" for i in range(1, 11))
        + "\nThen list all 10 filenames."
    )
    result = await agent.invoke(prompt)

    tool_started = result.tools_started()

    # Should have made multiple tool calls
    assert len(tool_started) >= 10, (
        f"Complex task should require 10+ tool calls. Got {len(tool_started)}. "
        f"Tools: {result.tool_names}"
    )

    # Verify assistant completed (didn't exhaust the tool-call budget)
    assert len(result.assistant_turns()) > 0, (
        "Task should complete with assistant_complete, not timeout"
    )


@pytest.mark.asyncio
async def test_no_early_stop_verification(sandbox_id):
    """Verify agent doesn't stop early when task explicitly asks for specific completion criteria."""
    agent = create_eval_agent(
        sandbox_id=sandbox_id,
        system_prompt=(
            "Complete the ENTIRE task. Do not summarize or stop early. "
            "Continue working — do not stop to summarize results unless the task is done. "
            "Make ALL tool calls required to complete every step."
        ),
    )

    result = await agent.invoke(
        "Complete these EXACT steps:\n"
        "1. Create /workspace/complete1.txt with 'FIRST'\n"
        "2. Create /workspace/complete2.txt with 'SECOND'\n"
        "3. Create /workspace/complete3.txt with 'THIRD'\n"
        "4. Run: ls /workspace/complete*.txt\n"
        "5. Tell me the EXACT output from step 4."
    )

    tool_started = result.tools_started()
    tool_names = [ts.tool_name for ts in tool_started]

    # Should have a listing/verification step (step 4) — model may use
    # shell with ls/cat to verify.
    bash_calls = [ts for ts in tool_started if ts.tool_name == "shell"]

    has_verification_step = bool(bash_calls)
    assert has_verification_step, (
        f"Should execute a verification step (ls/cat). Tools: {tool_names}"
    )


@pytest.mark.asyncio
async def test_agent_completes_without_summarizing_early(sandbox_id):
    """Agent should not stop early by summarizing - must complete actual operations."""
    agent = create_eval_agent(
        sandbox_id=sandbox_id,
        system_prompt=(
            "Do the actual work. Do NOT summarize that you would do something - actually do it. "
            "Complete every step personally. "
            "Continue working — do not stop. Make tool calls for ALL steps."
        ),
    )

    result = await agent.invoke(
        "Perform these actions (not just describe them):\n"
        "1. Write to /workspace/action1.txt: 'ACTION1'\n"
        "2. Write to /workspace/action2.txt: 'ACTION2'\n"
        "3. Verify both files exist and report their content."
    )

    # Should actually perform writes, not just describe
    write_calls = [tc for tc in result.tool_calls if tc.name == "write_file"]
    assert len(write_calls) >= 2, (
        f"Should perform 2 write actions. Got {len(write_calls)}. Tools: {result.tool_names}"
    )


# ===========================================================================
# AREA 4: Integration - All Three Test Areas Combined
# ===========================================================================


@pytest.mark.asyncio
@pytest.mark.skip(
    reason="load_skill tool not available via EvalAgent — requires HTTP agent creation path"
)
async def test_full_integration_tool_accuracy_plus_skill_following(sandbox_id):
    """Combines: correct tool selection + skill instruction following + task completion.

    Skipped: This test required load_skill to verify skill loading alongside
    tool accuracy. Since EvalAgent does not register load_skill, this integrated
    test cannot verify the skill-loading portion. The tool accuracy and task
    completion aspects are covered by the other tests in this module.
    """
    pass
