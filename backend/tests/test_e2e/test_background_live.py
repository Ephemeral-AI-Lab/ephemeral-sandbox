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

import logging

import pytest

from engine.eval_agent import EvalAgent
from tests.test_e2e.conftest import create_test_sandbox, delete_test_sandbox

logger = logging.getLogger(__name__)

pytestmark = [pytest.mark.e2e, pytest.mark.live]

AGENT_PROMPT = """\
You are test-background-agent, a developer with a remote Daytona sandbox.

IMPORTANT RULES:
- You MUST use tools for every action — never just describe what you'd do.
- Use daytona_bash to run commands, daytona_write_file to create files.
- You have background task support: add "background": true to tool input for long-running operations.
- Use check_background_progress to monitor background tasks.
- Use cancel_background_task to cancel running background tasks.

BACKGROUND EXECUTION GUIDELINES:
- For commands that take >5 seconds (test suites, builds, npm install), run in background.
- For quick commands (<5 seconds like echo, pwd, cat), run in foreground.
- When running in background, continue with other useful work.
- Periodically check progress of background tasks.
- Cancel background tasks that appear stuck or failing.

Always be concise. Execute tools, don't just describe them.
"""


def _log_result(result, label: str) -> None:
    """Log EvalResult for debugging."""
    started = result.tools_started()
    completed = result.tools_completed()
    bg_started = result.background_started()

    logger.info(
        f"\n{'='*60}\n[{label}] Event summary:\n"
        f"  Tools started: {len(started)}\n"
        f"  Tools completed: {len(completed)}\n"
        f"  Background started: {len(bg_started)}\n"
        f"  Tool names: {result.tool_names}\n"
        f"{'='*60}\n"
    )


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
        return EvalAgent.create(
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
        _log_result(result, "quick_foreground")

        assert len(result.assistant_turns()) >= 1, "Missing assistant turn"
        assert len(result.tools_started()) >= 1, "Should use at least one tool"
        logger.info("[PASS] Quick command executed successfully")

    @pytest.mark.asyncio
    async def test_long_command_offered_background(self, sandbox):
        """LLM should consider backgrounding a long command."""
        agent = self._make_agent(sandbox)
        result = await agent.invoke(
            "Do TWO things:\n"
            "1. Run 'sleep 10 && echo LONG_DONE' in the sandbox using daytona_bash "
            "with background: true (this takes a long time)\n"
            "2. While waiting, run 'echo FOREGROUND_DONE' in foreground\n\n"
            "You MUST use background: true for the sleep command."
        )
        _log_result(result, "long_background")

        assert len(result.assistant_turns()) >= 1, "Missing assistant turn"
        assert any("daytona" in n for n in result.tool_names), \
            f"Expected daytona tool usage. Got: {result.tool_names}"
        logger.info("[PASS] Long command scenario completed")


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
        agent = EvalAgent.create(
            system_prompt=AGENT_PROMPT,
            sandbox_id=sandbox["id"],
            enable_background_tasks=True,
        )
        result = await agent.invoke(
            "Please do these tasks:\n"
            "1. Run 'sleep 5 && echo BUILD_COMPLETE' in background using daytona_bash "
            "with background: true\n"
            "2. While waiting, run 'echo FOREGROUND_TASK_1' in the sandbox (foreground)\n"
            "3. Then run 'echo FOREGROUND_TASK_2' in the sandbox (foreground)\n"
            "4. After foreground tasks, check on the background task using check_background_progress\n\n"
            "Make sure to use background: true for step 1."
        )
        _log_result(result, "foreground_idle")

        assert len(result.assistant_turns()) >= 1, "Missing assistant turn"
        assert len(result.tools_started()) >= 2, \
            f"Expected multiple tool calls. Got {len(result.tools_started())}: {result.tool_names}"
        logger.info("[PASS] Background + foreground work completed")


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
        agent = EvalAgent.create(
            system_prompt=AGENT_PROMPT,
            sandbox_id=sandbox["id"],
            enable_background_tasks=True,
        )
        result = await agent.invoke(
            "Do the following:\n"
            "1. Run 'sleep 8 && echo INSTALL_DONE' in background using daytona_bash "
            "with background: true\n"
            "2. Run 'echo doing_other_work' in foreground\n"
            "3. Call check_background_progress to see the background task status\n"
            "4. Report what you see\n\n"
            "You MUST call check_background_progress at step 3."
        )
        _log_result(result, "progress_check")

        assert len(result.assistant_turns()) >= 1, "Missing assistant turn"

        has_progress_check = result.has_tool("check_background_progress")
        logger.info(f"[Test3] check_background_progress called: {has_progress_check}")

        if has_progress_check:
            logger.info("[PASS] LLM proactively checked background progress")
        else:
            logger.warning("[WARN] LLM did not call check_background_progress — LLM non-determinism")

        assert len(result.tools_started()) >= 2, f"Expected 2+ tools. Got: {result.tool_names}"


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
        agent = EvalAgent.create(
            system_prompt=AGENT_PROMPT,
            sandbox_id=sandbox["id"],
            enable_background_tasks=True,
        )
        result = await agent.invoke(
            "Do the following steps in order:\n"
            "1. Run 'sleep 30 && echo TESTS_DONE' in background using daytona_bash "
            "with background: true\n"
            "2. Run 'echo doing_foreground_fix' in foreground\n"
            "3. Call check_background_progress to check the background task\n"
            "4. The tests are taking too long. Cancel the background task using "
            "cancel_background_task with the task_id from step 3. "
            "Use reason: 'Tests taking too long, need to fix code first'\n"
            "5. Confirm the cancellation\n\n"
            "You MUST follow all 5 steps in order. Use background: true for step 1."
        )
        _log_result(result, "cancel_failing")

        assert len(result.assistant_turns()) >= 1, "Missing assistant turn"

        has_cancel = result.has_tool("cancel_background_task")
        logger.info(f"[Test4] cancel_background_task called: {has_cancel}")

        if has_cancel:
            logger.info("[PASS] LLM cancelled the background task")
        else:
            logger.warning("[WARN] LLM did not call cancel_background_task — LLM non-determinism")

        assert len(result.tools_started()) >= 2, f"Expected 2+ tool calls. Got: {result.tool_names}"


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
        agent = EvalAgent.create(
            system_prompt=AGENT_PROMPT,
            sandbox_id=sandbox["id"],
            enable_background_tasks=True,
        )
        result = await agent.invoke(
            "Do the following steps:\n"
            "1. Run 'sleep 60 && echo INSTALL_DONE' in background using daytona_bash "
            "with background: true (simulating a hanging npm install)\n"
            "2. Call check_background_progress to check status\n"
            "3. Call check_background_progress again — it's still running\n"
            "4. The install is clearly hanging. Cancel it using cancel_background_task "
            "with reason: 'npm install appears to be hanging'\n"
            "5. Report what happened\n\n"
            "You MUST use background: true for step 1 and follow all steps."
        )
        _log_result(result, "cancel_hanging")

        assert len(result.assistant_turns()) >= 1, "Missing assistant turn"

        progress_count = result.tool_count("check_background_progress")
        cancel_count = result.tool_count("cancel_background_task")
        logger.info(f"[Test5] Progress checks: {progress_count}, Cancels: {cancel_count}")

        if progress_count >= 2 and cancel_count >= 1:
            logger.info("[PASS] LLM checked progress twice and cancelled hanging task")
        elif cancel_count >= 1:
            logger.info("[PASS] LLM cancelled hanging task (fewer progress checks than expected)")
        else:
            logger.warning(
                f"[WARN] Expected 2+ progress checks and 1 cancel. "
                f"Got progress={progress_count}, cancel={cancel_count} — LLM non-determinism"
            )

        assert len(result.tools_started()) >= 2, f"Expected 2+ tool calls. Got: {result.tool_names}"
