# ruff: noqa
"""Live E2E: LLM autonomous background task decision-making.

Tests that the LLM independently decides when to check and cancel
background tasks — NO explicit instructions to check or cancel.

Uses EvalAgent for credential loading and agent configuration.
Run with: .venv/bin/python -m pytest backend/tests/test_e2e/test_background_autonomy_live.py -v -s --log-cli-level=INFO
"""

from __future__ import annotations

import pytest

from engine.testing.eval_agent import EvalAgent
from tests.test_e2e.bg_prompts import _build_prompt, _BG_GUIDELINES
from tests.test_e2e.conftest import create_eval_agent, create_test_sandbox, delete_test_sandbox
from tests.test_e2e.helpers import log_result

import logging
logger = logging.getLogger(__name__)

pytestmark = [pytest.mark.e2e, pytest.mark.live]

AGENT_PROMPT = _build_prompt(
    agent_name="test-autonomy-agent",
    extra_sections=(
        _BG_GUIDELINES
        + "\n- Use your own judgment on when to check or cancel background tasks."
    ),
)


# ===========================================================================
# Test 1: LLM decides on its own to check background progress
# ===========================================================================


@pytest.mark.skipif(not EvalAgent.has_all(), reason="API + Daytona both required")
class TestAutonomousProgressCheck:
    """No instruction to check — LLM decides on its own."""

    @pytest.fixture(scope="class")
    def sandbox(self):
        sb = create_test_sandbox("auto-check")
        yield sb
        delete_test_sandbox(sb["id"])

    @pytest.mark.asyncio
    async def test_llm_autonomously_checks_progress(self, sandbox):
        """Give the LLM a background task and foreground work.
        Do NOT tell it to check progress. See if it does on its own."""
        agent = create_eval_agent(
            system_prompt=AGENT_PROMPT,
            sandbox_id=sandbox["id"],
            enable_background_tasks=True,
        )
        result = await agent.invoke(
            "I need you to do two things:\n"
            "- Run a long build: 'sleep 20 && echo BUILD_OK' in background\n"
            "- While it runs, create a file /workspace/readme.txt with "
            "'Hello World' using daytona_shell: echo 'Hello World' > /workspace/readme.txt\n"
            "- Then read it back: cat /workspace/readme.txt\n\n"
            "Let me know when everything is done."
        )
        log_result(result,"autonomous_check")

        assert len(result.assistant_turns()) >= 1, "Missing assistant turn"
        assert result.has_tool_with_background("daytona_shell"), \
            f"Expected daytona_shell called with background: true. Got tool calls: {result.tool_calls}"
        assert len(result.background_started()) >= 1, \
            f"Expected BackgroundTaskStarted event. Got tools: {result.tool_names}"
        assert len(result.tool_names) >= 3, \
            f"Expected 3+ tools (background + foreground work). Got: {result.tool_names}"
        assert result.has_tool("check_background_progress"), \
            f"Expected LLM to autonomously check progress. Got: {result.tool_names}"
        logger.info("[PASS] LLM autonomously checked background progress")


# ===========================================================================
# Test 2: LLM decides on its own to cancel a hanging task
# ===========================================================================


@pytest.mark.skipif(not EvalAgent.has_all(), reason="API + Daytona both required")
class TestAutonomousCancel:
    """Background a task that will never finish. LLM must decide on its own."""

    @pytest.fixture(scope="class")
    def sandbox(self):
        sb = create_test_sandbox("auto-cancel")
        yield sb
        delete_test_sandbox(sb["id"])

    @pytest.mark.asyncio
    async def test_llm_autonomously_handles_long_task(self, sandbox):
        """Background a very long task. Give foreground work. See what happens."""
        agent = create_eval_agent(
            system_prompt=AGENT_PROMPT,
            sandbox_id=sandbox["id"],
            enable_background_tasks=True,
        )
        result = await agent.invoke(
            "Run 'sleep 120 && echo NEVER_FINISHES' in background.\n"
            "Then run 'echo quick_task_done' in foreground.\n\n"
            "The background task simulates a very slow npm install. "
            "Use your judgment on what to do about it."
        )
        log_result(result,"autonomous_cancel")

        assert len(result.assistant_turns()) >= 1, "Missing assistant turn"
        assert result.has_tool_with_background("daytona_shell"), \
            f"Expected daytona_shell called with background: true. Got tool calls: {result.tool_calls}"
        assert len(result.background_started()) >= 1, \
            f"Expected BackgroundTaskStarted event. Got tools: {result.tool_names}"
        assert len(result.tool_names) >= 2, \
            f"Expected 2+ tools (background + foreground). Got: {result.tool_names}"
        assert result.has_tool("check_background_progress"), \
            f"Expected LLM to autonomously check progress on long task. Got: {result.tool_names}"
        assert result.has_tool("cancel_background_task"), \
            f"Expected LLM to autonomously cancel the 120s task. Got: {result.tool_names}"
        logger.info("[PASS] LLM autonomously checked and cancelled the long task")


# ===========================================================================
# Test 3: Multi-task autonomy — LLM manages two background tasks
# ===========================================================================


@pytest.mark.skipif(not EvalAgent.has_all(), reason="API + Daytona both required")
class TestAutonomousMultiTask:
    """Two background tasks. LLM must manage them independently."""

    @pytest.fixture(scope="class")
    def sandbox(self):
        sb = create_test_sandbox("auto-multi")
        yield sb
        delete_test_sandbox(sb["id"])

    @pytest.mark.asyncio
    async def test_llm_manages_multiple_background_tasks(self, sandbox):
        """Two background tasks with different durations. See how LLM manages."""
        agent = create_eval_agent(
            system_prompt=AGENT_PROMPT,
            sandbox_id=sandbox["id"],
            enable_background_tasks=True,
        )
        result = await agent.invoke(
            "I need two things running in the background:\n"
            "- A fast build: 'sleep 10 && echo FAST_BUILD_DONE' in background\n"
            "- A slow test suite: 'sleep 60 && echo SLOW_TESTS_DONE' in background\n\n"
            "While those run, create /workspace/status.txt with 'waiting for builds' "
            "using daytona_shell.\n\n"
            "Manage the background tasks as you see fit."
        )
        log_result(result,"autonomous_multi")

        assert len(result.assistant_turns()) >= 1, "Missing assistant turn"
        assert result.has_tool_with_background("daytona_shell"), \
            f"Expected daytona_shell called with background: true. Got tool calls: {result.tool_calls}"
        assert len(result.background_started()) >= 2, \
            f"Expected 2+ BackgroundTaskStarted events (two background tasks). Got: {result.background_started()}"

        bash_calls = result.tool_count("daytona_shell")
        assert bash_calls >= 3, \
            f"Expected 3+ bash calls (2 background + 1 foreground). Got: {result.tool_names}"
        assert result.has_tool("check_background_progress"), \
            f"Expected LLM to autonomously check progress. Got: {result.tool_names}"
        logger.info(f"[PASS] Multi-task autonomy: {len(result.tool_names)} total tools")
