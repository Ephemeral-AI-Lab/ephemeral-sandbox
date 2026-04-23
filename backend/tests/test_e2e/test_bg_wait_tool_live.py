# ruff: noqa
"""Live E2E: wait_for_background_task tool — blocking until tasks complete, timeouts, ordering.

Tests thorough usage of wait_for_background_task across single-task waits, specific task IDs,
wait_for_all, timeout behavior, already-completed tasks, no-task scenarios, and check-then-wait
ordering.

Run with: .venv/bin/python -m pytest backend/tests/test_e2e/test_bg_wait_tool_live.py -v -s --log-cli-level=INFO
"""

from __future__ import annotations

import logging

import pytest

from engine.testing.eval_agent import EvalAgent
from tests.test_e2e.conftest import create_eval_agent, create_test_sandbox, delete_test_sandbox
from tests.test_e2e.helpers import log_result

logger = logging.getLogger(__name__)

pytestmark = [pytest.mark.e2e, pytest.mark.live]

AGENT_PROMPT = """\
You are test-wait-agent, a developer with a remote Daytona sandbox.

IMPORTANT RULES:
- You MUST use tools for every action — never just describe what you'd do.
- Use daytona_shell to run commands, daytona_write_file to create files.
- You have background task support: add "background": true to tool input for long-running operations.
- Use check_background_progress to get an instant status snapshot of background tasks.
- Use wait_for_background_task to block until background tasks complete (only when no foreground work remains).
- Use cancel_background_task to cancel running background tasks.

BACKGROUND WORKFLOW:
1. Launch background tasks with "background": true
2. Do any foreground work while background runs
3. When idle, call check_background_progress first, then wait_for_background_task
4. After wait completes, act on results or cancel slow tasks

Always be concise. Execute tools, don't just describe them.
"""


# ===========================================================================
# Test 1: Wait blocks until a single background task completes
# ===========================================================================


@pytest.mark.skipif(not EvalAgent.has_all(), reason="API + Daytona both required")
class TestWaitForSingleTask:
    """LLM uses wait_for_background_task to block until one task finishes."""

    @pytest.fixture(scope="class")
    def sandbox(self):
        sb = create_test_sandbox("wait-basic")
        yield sb
        delete_test_sandbox(sb["id"])

    @pytest.mark.asyncio
    async def test_wait_blocks_until_task_completes(self, sandbox):
        """Launch a background task, do foreground prep, check, then wait for completion."""
        agent = create_eval_agent(
            system_prompt=AGENT_PROMPT,
            sandbox_id=sandbox["id"],
            enable_background_tasks=True,
        )
        result = await agent.invoke(
            "Do these steps:\n"
            "1. Run 'sleep 5 && echo WAIT_RESULT_OK' in background (background: true)\n"
            "2. Run 'echo PREP' in foreground\n"
            "3. Call check_background_progress to see current status\n"
            "4. Call wait_for_background_task with timeout=15 to block until the task completes\n"
            "5. Report the output from the background task\n\n"
            "Use background: true for step 1 ONLY. "
            "You MUST call check_background_progress before wait_for_background_task."
        )
        log_result(result,"wait_single")

        assert result.has_tool_with_background("daytona_shell"), \
            f"Expected daytona_shell with background: true. Got: {result.tool_calls}"
        assert result.background_started() >= 1 if isinstance(result.background_started(), int) \
            else len(result.background_started()) >= 1, \
            f"Expected BackgroundTaskStarted event. Got: {result.tool_names}"
        assert result.has_tool("check_background_progress"), \
            f"Expected check_background_progress. Got: {result.tool_names}"
        assert result.has_tool("wait_for_background_task"), \
            f"Expected wait_for_background_task. Got: {result.tool_names}"

        # Wait call must have a reasonable timeout
        wait_calls = [tc for tc in result.tool_calls if tc.name == "wait_for_background_task"]
        assert wait_calls, "No wait_for_background_task call found"
        timeout_val = wait_calls[0].input.get("timeout")
        assert timeout_val is None or timeout_val >= 10, \
            f"Expected timeout >= 10 (or unset). Got: {timeout_val}"

        # LLM text should acknowledge the result
        text_lower = result.text.lower()
        assert any(word in text_lower for word in ["wait_result_ok", "complet", "done"]), \
            f"Expected text to contain result. Got: {result.text[:300]}"

        # check must precede wait in tool sequence
        check_indices = [i for i, tc in enumerate(result.tool_calls)
                         if tc.name == "check_background_progress"]
        wait_indices = [i for i, tc in enumerate(result.tool_calls)
                        if tc.name == "wait_for_background_task"]
        assert check_indices and wait_indices, \
            f"Need both check and wait calls. Got: {result.tool_names}"
        assert check_indices[0] < wait_indices[0], \
            f"check_background_progress must precede wait_for_background_task. " \
            f"check_idx={check_indices[0]}, wait_idx={wait_indices[0]}"

        assert not result.has_non_cancel_errors, \
            f"Unexpected errors: {[e.output[:200] for e in result.non_cancel_error_events]}"


# ===========================================================================
# Test 2: Wait for a specific task by task_id
# ===========================================================================


@pytest.mark.skipif(not EvalAgent.has_all(), reason="API + Daytona both required")
class TestWaitWithSpecificTaskId:
    """LLM waits for a specific task by ID while cancelling the other."""

    @pytest.fixture(scope="class")
    def sandbox(self):
        sb = create_test_sandbox("wait-specific")
        yield sb
        delete_test_sandbox(sb["id"])

    @pytest.mark.asyncio
    async def test_wait_for_specific_task_by_id(self, sandbox):
        """Launch two bg tasks, wait for the short one by ID, cancel the slow one."""
        agent = create_eval_agent(
            system_prompt=AGENT_PROMPT,
            sandbox_id=sandbox["id"],
            enable_background_tasks=True,
        )
        result = await agent.invoke(
            "Do these steps:\n"
            "1. Run 'sleep 15 && echo TASK_A_DONE' in background (background: true) — this is TASK A (short)\n"
            "2. Run 'sleep 120 && echo TASK_B_DONE' in background (background: true) — this is TASK B (long)\n"
            "3. Run 'echo READY' in foreground\n"
            "4. Call check_background_progress to see both tasks and get their task IDs\n"
            "5. Call wait_for_background_task passing the task_id of TASK A (the short one) "
            "with timeout=25. Wait only for that specific task.\n"
            "6. After TASK A completes, cancel TASK B using cancel_background_task\n"
            "7. Report the results of both tasks\n\n"
            "Use background: true for steps 1 and 2 ONLY. "
            "You MUST pass task_id to wait_for_background_task to wait for TASK A specifically."
        )
        log_result(result,"wait_specific_id")

        bg_bash = [tc for tc in result.tool_calls
                   if tc.name == "daytona_shell" and tc.input.get("background") is True]
        assert len(bg_bash) >= 2, \
            f"Expected 2+ daytona_shell with background: true. Got {len(bg_bash)}"
        assert len(result.background_started()) >= 2, \
            f"Expected 2+ BackgroundTaskStarted events. Got {len(result.background_started())}"

        assert result.has_tool("check_background_progress"), \
            f"Expected check_background_progress. Got: {result.tool_names}"
        assert result.has_tool("wait_for_background_task"), \
            f"Expected wait_for_background_task. Got: {result.tool_names}"

        # Wait call must have a task_id set
        wait_calls = [tc for tc in result.tool_calls if tc.name == "wait_for_background_task"]
        assert wait_calls, "No wait_for_background_task call found"
        task_id_val = wait_calls[0].input.get("task_id")
        assert task_id_val is not None, \
            f"Expected wait_for_background_task to have task_id set. Got input: {wait_calls[0].input}"

        assert result.has_tool("cancel_background_task"), \
            f"Expected cancel_background_task for the slow task. Got: {result.tool_names}"

        text_lower = result.text.lower()
        assert "task" in text_lower or "task_a" in text_lower or "task_b" in text_lower, \
            f"Expected text to mention tasks. Got: {result.text[:300]}"

        # Use unrecovered_errors: the LLM occasionally fumbles the task_id
        # plumbing (passes None to cancel_background_task) and then retries
        # successfully. Those recovered failures should not fail the test,
        # only errors the agent never recovered from.
        assert not result.has_unrecovered_errors, \
            f"Unrecovered errors: {[e.output[:200] for e in result.unrecovered_error_events]}"


# ===========================================================================
# Test 3: Wait for all background tasks at once
# ===========================================================================


@pytest.mark.skipif(not EvalAgent.has_all(), reason="API + Daytona both required")
class TestWaitForAllTasks:
    """LLM uses wait_for_all=True to block until every background task completes."""

    @pytest.fixture(scope="class")
    def sandbox(self):
        sb = create_test_sandbox("wait-all")
        yield sb
        delete_test_sandbox(sb["id"])

    @pytest.mark.asyncio
    async def test_wait_for_all_blocks_until_all_complete(self, sandbox):
        """Launch two bg tasks, then wait for ALL of them with task_id="all"."""
        agent = create_eval_agent(
            system_prompt=AGENT_PROMPT,
            sandbox_id=sandbox["id"],
            enable_background_tasks=True,
        )
        result = await agent.invoke(
            "Do these steps:\n"
            "1. Run 'sleep 3 && echo FIRST_DONE' in background (background: true)\n"
            "2. Run 'sleep 6 && echo SECOND_DONE' in background (background: true)\n"
            "3. Run 'echo STARTING' in foreground\n"
            "4. Call check_background_progress to see current status\n"
            "5. Call wait_for_background_task with task_id=\"all\" and timeout=15 "
            "to block until BOTH tasks complete\n"
            "6. Report which tasks completed and what their outputs were\n\n"
            "Use background: true for steps 1 and 2 ONLY. "
            "You MUST use task_id=\"all\" in step 5."
        )
        log_result(result,"wait_for_all")

        bg_bash = [tc for tc in result.tool_calls
                   if tc.name == "daytona_shell" and tc.input.get("background") is True]
        assert len(bg_bash) >= 2, \
            f"Expected 2+ daytona_shell with background: true. Got {len(bg_bash)}"
        assert len(result.background_started()) >= 2, \
            f"Expected 2+ BackgroundTaskStarted events. Got {len(result.background_started())}"

        assert result.has_tool("wait_for_background_task"), \
            f"Expected wait_for_background_task. Got: {result.tool_names}"

        # Wait call must have wait_for_all=True
        wait_calls = [tc for tc in result.tool_calls if tc.name == "wait_for_background_task"]
        assert wait_calls, "No wait_for_background_task call found"
        assert wait_calls[0].input.get("task_id") == "all", \
            f"Expected task_id=\"all\". Got input: {wait_calls[0].input}"

        text_lower = result.text.lower()
        first_mentioned = any(w in text_lower for w in ["first_done", "first"])
        second_mentioned = any(w in text_lower for w in ["second_done", "second", "both", "all"])
        assert first_mentioned and second_mentioned, \
            f"Expected text to mention both tasks. Got: {result.text[:300]}"

        assert not result.has_non_cancel_errors, \
            f"Unexpected errors: {[e.output[:200] for e in result.non_cancel_error_events]}"


# ===========================================================================
# Test 4: Wait times out — task didn't finish in time
# ===========================================================================


@pytest.mark.skipif(not EvalAgent.has_all(), reason="API + Daytona both required")
class TestWaitTimeout:
    """Wait times out on a long-running task; LLM then cancels it."""

    @pytest.fixture(scope="class")
    def sandbox(self):
        sb = create_test_sandbox("wait-timeout")
        yield sb
        delete_test_sandbox(sb["id"])

    @pytest.mark.asyncio
    async def test_wait_timeout_returns_status(self, sandbox):
        """Background a very long task, wait with short timeout, then cancel."""
        agent = create_eval_agent(
            system_prompt=AGENT_PROMPT,
            sandbox_id=sandbox["id"],
            enable_background_tasks=True,
        )
        result = await agent.invoke(
            "Do these steps:\n"
            "1. Run 'sleep 60 && echo NEVER_FINISH' in background (background: true)\n"
            "2. Run 'echo FG_DONE' in foreground\n"
            "3. Call check_background_progress to see the task status\n"
            "4. Call wait_for_background_task with timeout=5. The task will NOT finish in 5 seconds.\n"
            "5. Report what wait_for_background_task returned — did it timeout?\n"
            "6. Cancel the background task using cancel_background_task\n\n"
            "Use background: true for step 1 ONLY. "
            "Use a SHORT timeout (5 seconds) in step 4 — the task should time out."
        )
        log_result(result,"wait_timeout")

        assert result.has_tool("wait_for_background_task"), \
            f"Expected wait_for_background_task. Got: {result.tool_names}"

        # Wait call must have a short timeout
        wait_calls = [tc for tc in result.tool_calls if tc.name == "wait_for_background_task"]
        assert wait_calls, "No wait_for_background_task call found"
        timeout_val = wait_calls[0].input.get("timeout")
        assert timeout_val is not None and timeout_val <= 10, \
            f"Expected timeout <= 10 (short timeout). Got: {timeout_val}"

        assert result.has_tool("cancel_background_task"), \
            f"Expected cancel_background_task after timeout. Got: {result.tool_names}"

        text_lower = result.text.lower()
        assert any(word in text_lower for word in ["timeout", "timed", "still running", "cancel"]), \
            f"Expected text to mention timeout/cancel. Got: {result.text[:300]}"

        assert not result.has_non_cancel_errors, \
            f"Unexpected errors: {[e.output[:200] for e in result.non_cancel_error_events]}"


# ===========================================================================
# Test 5: Wait on already-completed task returns immediately
# ===========================================================================


@pytest.mark.skipif(not EvalAgent.has_all(), reason="API + Daytona both required")
class TestWaitAlreadyCompleted:
    """Calling wait on an already-finished task should return immediately with the result."""

    @pytest.fixture(scope="class")
    def sandbox(self):
        sb = create_test_sandbox("wait-already-done")
        yield sb
        delete_test_sandbox(sb["id"])

    @pytest.mark.asyncio
    async def test_wait_on_already_completed_task_returns_immediately(self, sandbox):
        """Launch instant bg task, do foreground sleep, check it's done, then wait."""
        agent = create_eval_agent(
            system_prompt=AGENT_PROMPT,
            sandbox_id=sandbox["id"],
            enable_background_tasks=True,
        )
        result = await agent.invoke(
            "Do these steps:\n"
            "1. Run 'echo INSTANT_DONE' in background (background: true) — this completes immediately\n"
            "2. Run 'sleep 2 && echo WAITED' in foreground — give the background task time to finish\n"
            "3. Call check_background_progress — the background task should already be done\n"
            "4. Call wait_for_background_task — it should return immediately since the task is already done\n"
            "5. Report what you found — was the task already completed when you waited?\n\n"
            "Use background: true for step 1 ONLY."
        )
        log_result(result,"wait_already_done")

        assert result.has_tool("check_background_progress"), \
            f"Expected check_background_progress. Got: {result.tool_names}"
        assert result.has_tool("wait_for_background_task"), \
            f"Expected wait_for_background_task. Got: {result.tool_names}"

        text_lower = result.text.lower()
        assert any(word in text_lower for word in ["instant_done", "complet", "already"]), \
            f"Expected text to confirm task was completed. Got: {result.text[:300]}"

        assert not result.has_non_cancel_errors, \
            f"Unexpected errors: {[e.output[:200] for e in result.non_cancel_error_events]}"


# ===========================================================================
# Test 6: Wait with no background tasks returns gracefully
# ===========================================================================


@pytest.mark.skipif(not EvalAgent.has_all(), reason="API + Daytona both required")
class TestWaitNoBackgroundTasks:
    """Calling wait_for_background_task with no running tasks should return gracefully."""

    @pytest.fixture(scope="class")
    def sandbox(self):
        sb = create_test_sandbox("wait-none")
        yield sb
        delete_test_sandbox(sb["id"])

    @pytest.mark.asyncio
    async def test_wait_with_no_background_tasks(self, sandbox):
        """No background tasks launched — wait should return immediately with empty/none result."""
        agent = create_eval_agent(
            system_prompt=AGENT_PROMPT,
            sandbox_id=sandbox["id"],
            enable_background_tasks=True,
        )
        result = await agent.invoke(
            "Do these steps:\n"
            "1. Run 'echo HELLO' in foreground only — do NOT use background: true\n"
            "2. Call wait_for_background_task even though there are no background tasks\n"
            "3. Report what the tool returned\n\n"
            "Do NOT launch any background tasks. Run everything in foreground."
        )
        log_result(result,"wait_none")

        assert result.has_tool("wait_for_background_task"), \
            f"Expected wait_for_background_task. Got: {result.tool_names}"

        # No background launches should have happened
        bg_bash = [tc for tc in result.tool_calls
                   if tc.name == "daytona_shell" and tc.input.get("background") is True]
        assert len(bg_bash) == 0, \
            f"Expected no background launches. Got {len(bg_bash)}: {result.tool_names}"
        assert len(result.background_started()) == 0, \
            f"Expected no BackgroundTaskStarted events. Got {len(result.background_started())}"

        text_lower = result.text.lower()
        assert any(word in text_lower for word in ["no", "none", "no background"]), \
            f"Expected text to mention no tasks. Got: {result.text[:300]}"

        assert not result.has_non_cancel_errors, \
            f"Unexpected errors: {[e.output[:200] for e in result.non_cancel_error_events]}"


# ===========================================================================
# Test 7: check_background_progress must precede wait_for_background_task
# ===========================================================================


@pytest.mark.skipif(not EvalAgent.has_all(), reason="API + Daytona both required")
class TestCheckThenWaitOrdering:
    """LLM must call check_background_progress before wait_for_background_task."""

    @pytest.fixture(scope="class")
    def sandbox(self):
        sb = create_test_sandbox("wait-ordering")
        yield sb
        delete_test_sandbox(sb["id"])

    @pytest.mark.asyncio
    async def test_check_must_precede_wait(self, sandbox):
        """Enforce that check always comes before wait in the tool call sequence."""
        agent = create_eval_agent(
            system_prompt=AGENT_PROMPT,
            sandbox_id=sandbox["id"],
            enable_background_tasks=True,
        )
        result = await agent.invoke(
            "Do these steps in EXACT order:\n"
            "1. Run 'sleep 5 && echo ORDER_TEST' in background (background: true)\n"
            "2. Run 'echo FG' in foreground\n"
            "3. Call check_background_progress FIRST — you MUST check before waiting\n"
            "4. Call wait_for_background_task with timeout=10 SECOND — only after checking\n"
            "5. Report the result\n\n"
            "Use background: true for step 1 ONLY. "
            "CRITICAL: check_background_progress MUST come before wait_for_background_task."
        )
        log_result(result,"wait_ordering")

        assert result.has_tool("check_background_progress"), \
            f"Expected check_background_progress. Got: {result.tool_names}"
        assert result.has_tool("wait_for_background_task"), \
            f"Expected wait_for_background_task. Got: {result.tool_names}"

        # check must come before wait in the tool sequence
        check_indices = [i for i, tc in enumerate(result.tool_calls)
                         if tc.name == "check_background_progress"]
        wait_indices = [i for i, tc in enumerate(result.tool_calls)
                        if tc.name == "wait_for_background_task"]
        assert check_indices and wait_indices, \
            f"Need both check and wait calls. Got: {result.tool_names}"
        assert check_indices[0] < wait_indices[0], \
            f"check_background_progress must precede wait_for_background_task. " \
            f"check_idx={check_indices[0]}, wait_idx={wait_indices[0]}"

        text_lower = result.text.lower()
        assert any(word in text_lower for word in ["order_test", "complet", "done"]), \
            f"Expected text to contain result. Got: {result.text[:300]}"

        assert not result.has_non_cancel_errors, \
            f"Unexpected errors: {[e.output[:200] for e in result.non_cancel_error_events]}"
