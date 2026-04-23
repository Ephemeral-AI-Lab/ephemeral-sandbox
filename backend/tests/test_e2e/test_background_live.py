# ruff: noqa
"""Live E2E: Background task execution with real LLM.

Tests that a real LLM correctly:
1. Decides whether to background a tool or run foreground
2. Does foreground work while background runs
3. Proactively calls check_background_progress
4. Cancels a background task after seeing issues
5. Cancels a hanging background task after repeated progress checks

Uses EvalAgent for credential loading and agent configuration.
Run with: .venv/bin/python -m pytest backend/tests/test_e2e/test_background_live.py -v -s --log-cli-level=INFO
"""

from __future__ import annotations

import pytest

from engine.testing.eval_agent import EvalAgent
from tests.test_e2e.bg_prompts import BG_STANDARD
from tests.test_e2e.conftest import create_eval_agent, create_test_sandbox, delete_test_sandbox
from tests.test_e2e.helpers import log_result

pytestmark = [pytest.mark.e2e, pytest.mark.live]

AGENT_PROMPT = BG_STANDARD


# ===========================================================================
# Test 1: LLM decides foreground vs background
# ===========================================================================


@pytest.mark.skipif(not EvalAgent.has_all(), reason="API + Daytona both required")
class TestLLMBackgroundDecision:
    """Test that the LLM decides appropriately between foreground and background."""

    @pytest.fixture(scope="class")
    def sandbox(self):
        sb = create_test_sandbox("bg-decision")
        yield sb
        delete_test_sandbox(sb["id"])

    def _make_agent(self, sandbox):
        return create_eval_agent(
            system_prompt=AGENT_PROMPT,
            sandbox_id=sandbox["id"],
            enable_background_tasks=True,
        )

    @pytest.mark.asyncio
    async def test_quick_command_runs_foreground(self, sandbox):
        """LLM should run a fast command in foreground (no background flag)."""
        agent = self._make_agent(sandbox)
        result = await agent.invoke(
            "Run this quick command in the sandbox: echo 'HELLO_FOREGROUND'. "
            "This is a fast command, do NOT run it in background."
        )
        log_result(result, "quick_foreground")

        assert len(result.assistant_turns()) >= 1, "Missing assistant turn"
        assert len(result.tools_started()) >= 1, "Should use at least one tool"
        assert len(result.background_started()) == 0, \
            "Quick command should NOT be backgrounded"

    @pytest.mark.asyncio
    async def test_long_command_offered_background(self, sandbox):
        """LLM should background a long command."""
        agent = self._make_agent(sandbox)
        result = await agent.invoke(
            "Do TWO things:\n"
            "1. Run 'sleep 10 && echo LONG_DONE' in the sandbox using daytona_shell "
            "with background: true (this takes a long time)\n"
            "2. While waiting, run 'echo FOREGROUND_DONE' in foreground\n\n"
            "You MUST use background: true for the sleep command."
        )
        log_result(result, "long_background")

        assert len(result.assistant_turns()) >= 1, "Missing assistant turn"
        assert result.has_tool_with_background("daytona_shell"), \
            f"Expected daytona_shell called with background: true. Got tool calls: {result.tool_calls}"
        assert len(result.background_started()) >= 1, \
            f"Expected BackgroundTaskStarted event. Got tools: {result.tool_names}"


# ===========================================================================
# Test 2: Foreground work while background runs + idle notification
# ===========================================================================


@pytest.mark.skipif(not EvalAgent.has_all(), reason="API + Daytona both required")
class TestForegroundAndIdleWait:
    """LLM does foreground work while background runs, gets result on idle."""

    @pytest.fixture(scope="class")
    def sandbox(self):
        sb = create_test_sandbox("bg-idle")
        yield sb
        delete_test_sandbox(sb["id"])

    @pytest.mark.asyncio
    async def test_background_with_foreground_work(self, sandbox):
        """LLM backgrounds a slow command, does foreground work, gets result."""
        agent = create_eval_agent(
            system_prompt=AGENT_PROMPT,
            sandbox_id=sandbox["id"],
            enable_background_tasks=True,
        )
        result = await agent.invoke(
            "Please do these tasks:\n"
            "1. Run 'sleep 5 && echo BUILD_COMPLETE' in background using daytona_shell "
            "with background: true\n"
            "2. While waiting, run 'echo FOREGROUND_TASK_1' in the sandbox (foreground)\n"
            "3. Then run 'echo FOREGROUND_TASK_2' in the sandbox (foreground)\n"
            "4. After foreground tasks, check on the background task using check_background_progress\n\n"
            "Make sure to use background: true for step 1."
        )
        log_result(result, "foreground_idle")

        assert len(result.assistant_turns()) >= 1, "Missing assistant turn"
        assert result.has_tool_with_background("daytona_shell"), \
            f"Expected daytona_shell called with background: true. Got tool calls: {result.tool_calls}"
        assert len(result.background_started()) >= 1, \
            f"Expected BackgroundTaskStarted event. Got tools: {result.tool_names}"
        assert len(result.tools_started()) >= 2, \
            f"Expected foreground work while background runs. Got: {result.tool_names}"
        assert result.has_tool("check_background_progress"), \
            f"Expected check_background_progress call. Got: {result.tool_names}"


# ===========================================================================
# Test 3: LLM proactively checks background progress
# ===========================================================================


@pytest.mark.skipif(not EvalAgent.has_all(), reason="API + Daytona both required")
class TestProactiveProgressCheck:
    """LLM proactively checks on background task status."""

    @pytest.fixture(scope="class")
    def sandbox(self):
        sb = create_test_sandbox("bg-progress")
        yield sb
        delete_test_sandbox(sb["id"])

    @pytest.mark.asyncio
    async def test_llm_checks_progress(self, sandbox):
        """LLM backgrounds a task and proactively calls check_background_progress."""
        agent = create_eval_agent(
            system_prompt=AGENT_PROMPT,
            sandbox_id=sandbox["id"],
            enable_background_tasks=True,
        )
        result = await agent.invoke(
            "Do the following:\n"
            "1. Run 'sleep 8 && echo INSTALL_DONE' in background using daytona_shell "
            "with background: true\n"
            "2. Run 'echo doing_other_work' in foreground\n"
            "3. Call check_background_progress to see the background task status\n"
            "4. Report what you see\n\n"
            "You MUST call check_background_progress at step 3."
        )
        log_result(result, "progress_check")

        assert len(result.assistant_turns()) >= 1, "Missing assistant turn"
        assert result.has_tool_with_background("daytona_shell"), \
            f"Expected daytona_shell called with background: true. Got tool calls: {result.tool_calls}"
        assert len(result.background_started()) >= 1, \
            f"Expected BackgroundTaskStarted event. Got tools: {result.tool_names}"
        assert result.has_tool("check_background_progress"), \
            f"Expected check_background_progress call. Got: {result.tool_names}"


# ===========================================================================
# Test 4: LLM cancels background task (failing tests)
# ===========================================================================


@pytest.mark.skipif(not EvalAgent.has_all(), reason="API + Daytona both required")
class TestCancelFailingTask:
    """LLM cancels a background task that's running a failing test suite."""

    @pytest.fixture(scope="class")
    def sandbox(self):
        sb = create_test_sandbox("bg-cancel-fail")
        yield sb
        delete_test_sandbox(sb["id"])

    @pytest.mark.asyncio
    async def test_llm_cancels_after_checking(self, sandbox):
        """LLM backgrounds a task, checks progress, then cancels it."""
        agent = create_eval_agent(
            system_prompt=AGENT_PROMPT,
            sandbox_id=sandbox["id"],
            enable_background_tasks=True,
        )
        result = await agent.invoke(
            "Do the following steps in order:\n"
            "1. Run 'sleep 30 && echo TESTS_DONE' in background using daytona_shell "
            "with background: true\n"
            "2. Run 'echo doing_foreground_fix' in foreground\n"
            "3. Call check_background_progress to check the background task\n"
            "4. The tests are taking too long. Cancel the background task using "
            "cancel_background_task with the task_id from step 3. "
            "Use reason: 'Tests taking too long, need to fix code first'\n"
            "5. Confirm the cancellation\n\n"
            "You MUST follow all 5 steps in order. Use background: true for step 1."
        )
        log_result(result, "cancel_failing")

        assert len(result.assistant_turns()) >= 1, "Missing assistant turn"
        assert result.has_tool_with_background("daytona_shell"), \
            f"Expected daytona_shell called with background: true. Got tool calls: {result.tool_calls}"
        assert len(result.background_started()) >= 1, \
            f"Expected BackgroundTaskStarted event. Got tools: {result.tool_names}"
        assert result.has_tool("check_background_progress"), \
            f"Expected check_background_progress call. Got: {result.tool_names}"
        assert result.has_tool("cancel_background_task"), \
            f"Expected cancel_background_task call. Got: {result.tool_names}"


# ===========================================================================
# Test 5: LLM cancels hanging task after repeated checks
# ===========================================================================


@pytest.mark.skipif(not EvalAgent.has_all(), reason="API + Daytona both required")
class TestCancelHangingTask:
    """LLM cancels a background task that appears to be hanging."""

    @pytest.fixture(scope="class")
    def sandbox(self):
        sb = create_test_sandbox("bg-cancel-hang")
        yield sb
        delete_test_sandbox(sb["id"])

    @pytest.mark.asyncio
    async def test_llm_cancels_hanging_install(self, sandbox):
        """LLM backgrounds a hanging command, checks twice, then cancels."""
        agent = create_eval_agent(
            system_prompt=AGENT_PROMPT,
            sandbox_id=sandbox["id"],
            enable_background_tasks=True,
        )
        result = await agent.invoke(
            "Do the following steps:\n"
            "1. Run 'sleep 60 && echo INSTALL_DONE' in background using daytona_shell "
            "with background: true (simulating a hanging npm install)\n"
            "2. Call check_background_progress to check status\n"
            "3. Call check_background_progress again — it's still running\n"
            "4. The install is clearly hanging. Cancel it using cancel_background_task "
            "with reason: 'npm install appears to be hanging'\n"
            "5. Report what happened\n\n"
            "You MUST use background: true for step 1 and follow all steps."
        )
        log_result(result, "cancel_hanging")

        assert len(result.assistant_turns()) >= 1, "Missing assistant turn"
        assert result.has_tool_with_background("daytona_shell"), \
            f"Expected daytona_shell called with background: true. Got tool calls: {result.tool_calls}"
        assert len(result.background_started()) >= 1, \
            f"Expected BackgroundTaskStarted event. Got tools: {result.tool_names}"

        progress_count = result.tool_count("check_background_progress")
        cancel_count = result.tool_count("cancel_background_task")
        logger.info(f"[Test5] Progress checks: {progress_count}, Cancels: {cancel_count}")

        assert progress_count >= 1, \
            f"Expected at least 1 progress check. Got: {result.tool_names}"
        assert cancel_count >= 1, \
            f"Expected cancel_background_task call. Got: {result.tool_names}"
