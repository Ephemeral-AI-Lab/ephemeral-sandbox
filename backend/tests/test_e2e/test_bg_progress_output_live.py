# ruff: noqa
"""Live E2E: Background task progress checking and output handling.

Tests check_background_progress, wait_for_background_task output truncation,
multi-task status visibility, and task-id-based filtering.

Run with: .venv/bin/python -m pytest backend/tests/test_e2e/test_bg_progress_output_live.py -v -s --log-cli-level=INFO
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
You are test-progress-agent, a developer with a remote Daytona sandbox.

IMPORTANT RULES:
- You MUST use tools for every action — never just describe what you'd do.
- Use daytona_shell to run commands, daytona_write_file to create files.
- You have background task support: add "background": true to tool input for long-running operations.
- Use check_background_progress to get an instant status snapshot of background tasks.
  For streaming-capable tools (like daytona_shell), check_background_progress now also
  returns a LIVE TAIL of stdout lines emitted so far while the task is still running.
  Use this live tail to make autonomous decisions: cancel early if you spot a fatal
  error line, or keep waiting if the task is still making progress.
- Use wait_for_background_task to block until background tasks complete (only when no foreground work remains).
- Use cancel_background_task to cancel running background tasks.

BACKGROUND WORKFLOW:
1. Launch background tasks with "background": true
2. Do any foreground work while background runs
3. Call check_background_progress for quick status snapshots
4. When idle with no foreground work, use wait_for_background_task to block
5. Use cancel_background_task for tasks taking too long

Always be concise. Execute tools, don't just describe them.
"""


# ===========================================================================
# Test 1: check_background_progress shows running task with elapsed time
# ===========================================================================


@pytest.mark.skipif(not EvalAgent.has_all(), reason="API + Daytona both required")
class TestCheckProgressRunningStatus:
    """LLM checks progress of a running background task and sees elapsed time."""

    @pytest.fixture(scope="class")
    def sandbox(self):
        sb = create_test_sandbox("prog-running")
        yield sb
        delete_test_sandbox(sb["id"])

    @pytest.mark.asyncio
    async def test_check_shows_running_tasks_with_elapsed(self, sandbox):
        """Launch a long task, do fg work, check progress to see running status with
        a live tail of streamed output, then cancel."""
        agent = create_eval_agent(
            system_prompt=AGENT_PROMPT,
            sandbox_id=sandbox["id"],
            enable_background_tasks=True,
        )
        result = await agent.invoke(
            "Launch this in background (background: true): "
            "'for i in $(seq 1 20); do echo \"tick_$i\"; sleep 2; done && echo LONG_TASK'. "
            "Do 'sleep 6' in foreground so the bg task has time to stream a few ticks. "
            "Now call check_background_progress. The task should show as 'running' "
            "with elapsed time AND you should see a live tail containing tick_N lines. "
            "Then cancel it with cancel_background_task using reason 'Test complete'. "
            "Report which tick_N lines you saw in the live tail."
        )
        log_result(result,"running_status")

        assert result.has_tool("check_background_progress"), \
            f"Expected check_background_progress. Got: {result.tool_names}"
        assert result.has_tool("cancel_background_task"), \
            f"Expected cancel_background_task. Got: {result.tool_names}"
        text_lower = result.text.lower()
        assert any(word in text_lower for word in ["running", "elapsed", "progress", "tick_"]), \
            f"Expected text to mention running/elapsed/progress/tick_. Got: {result.text[:300]}"

        # Live-tail assertion: at least one mid-flight check_background_progress
        # completion event must have surfaced partial tick_ lines while the bg
        # task was still running, and must NOT contain LONG_TASK (the final marker).
        check_completions = [
            e for e in result.tools_completed() if e.tool_name == "check_background_progress"
        ]
        saw_live_tail = any(
            '"status": "running"' in (e.output or "")
            and any(f"tick_{i}" in (e.output or "") for i in range(1, 6))
            and "LONG_TASK" not in (e.output or "")
            for e in check_completions
        )
        assert saw_live_tail, (
            "No mid-flight check_background_progress call surfaced live-tail tick_ lines. "
            f"Outputs: {[(e.output or '')[:300] for e in check_completions]}"
        )

        assert not result.has_non_cancel_errors, \
            f"Unexpected errors: {[e.output[:200] for e in result.non_cancel_error_events]}"


# ===========================================================================
# Test 2: check_background_progress shows completed task output
# ===========================================================================


@pytest.mark.skipif(not EvalAgent.has_all(), reason="API + Daytona both required")
class TestCheckProgressCompletedOutput:
    """LLM waits for a task to complete, then checks progress to see output."""

    @pytest.fixture(scope="class")
    def sandbox(self):
        sb = create_test_sandbox("prog-completed")
        yield sb
        delete_test_sandbox(sb["id"])

    @pytest.mark.asyncio
    async def test_check_shows_completed_task_output(self, sandbox):
        """Launch a short task with output, wait for it, then check progress for the output."""
        agent = create_eval_agent(
            system_prompt=AGENT_PROMPT,
            sandbox_id=sandbox["id"],
            enable_background_tasks=True,
        )
        result = await agent.invoke(
            "Launch 'sleep 3 && echo \"LINE1\\nLINE2\\nLINE3\\nRESULT_OK\"' in background (background: true). "
            "Do 'echo WAITING' in foreground. "
            "Use wait_for_background_task with timeout=10 to wait for it to complete. "
            "Then call check_background_progress to see the completed output. "
            "Report what lines you see in the output."
        )
        log_result(result,"completed_output")

        assert result.has_tool("wait_for_background_task"), \
            f"Expected wait_for_background_task. Got: {result.tool_names}"
        assert result.has_tool("check_background_progress"), \
            f"Expected check_background_progress. Got: {result.tool_names}"
        text_lower = result.text.lower()
        assert any(word in text_lower for word in ["result_ok", "line", "output", "complet"]), \
            f"Expected text to mention RESULT_OK/LINE/output/complet. Got: {result.text[:300]}"
        assert not result.has_non_cancel_errors, \
            f"Unexpected errors: {[e.output[:200] for e in result.non_cancel_error_events]}"


# ===========================================================================
# Test 3: check_background_progress with last_n_lines truncation
# ===========================================================================


@pytest.mark.skipif(not EvalAgent.has_all(), reason="API + Daytona both required")
class TestCheckProgressLastNLines:
    """LLM uses last_n_lines parameter on check_background_progress."""

    @pytest.fixture(scope="class")
    def sandbox(self):
        sb = create_test_sandbox("prog-lastn")
        yield sb
        delete_test_sandbox(sb["id"])

    @pytest.mark.asyncio
    async def test_check_with_last_n_lines_truncation(self, sandbox):
        """Launch a task that produces 50 lines, wait for it, then check with last_n_lines=5."""
        agent = create_eval_agent(
            system_prompt=AGENT_PROMPT,
            sandbox_id=sandbox["id"],
            enable_background_tasks=True,
        )
        result = await agent.invoke(
            "Launch this command in background (background: true): "
            "'for i in $(seq 1 50); do echo \"LOG_LINE_$i\"; done'. "
            "Wait for it with wait_for_background_task timeout=10. "
            "Then call check_background_progress with last_n_lines=5. "
            "Report how many lines you see in the output."
        )
        log_result(result,"last_n_lines")

        assert result.has_tool("wait_for_background_task"), \
            f"Expected wait_for_background_task. Got: {result.tool_names}"
        assert result.has_tool("check_background_progress"), \
            f"Expected check_background_progress. Got: {result.tool_names}"
        # Verify the check call included last_n_lines in its input
        check_calls = [tc for tc in result.tool_calls if tc.name == "check_background_progress"]
        assert any("last_n_lines" in tc.input for tc in check_calls), \
            f"Expected at least one check_background_progress call with last_n_lines. " \
            f"Got inputs: {[tc.input for tc in check_calls]}"
        text_lower = result.text.lower()
        assert any(word in text_lower for word in ["lines", "output", "truncat"]), \
            f"Expected text to mention lines/output/truncat. Got: {result.text[:300]}"
        assert not result.has_non_cancel_errors, \
            f"Unexpected errors: {[e.output[:200] for e in result.non_cancel_error_events]}"


# ===========================================================================
# Test 4: check_background_progress shows all tasks' status
# ===========================================================================


@pytest.mark.skipif(not EvalAgent.has_all(), reason="API + Daytona both required")
class TestCheckProgressMultipleTasks:
    """LLM launches 2 tasks and sees both in check_background_progress."""

    @pytest.fixture(scope="class")
    def sandbox(self):
        sb = create_test_sandbox("prog-multi")
        yield sb
        delete_test_sandbox(sb["id"])

    @pytest.mark.asyncio
    async def test_check_shows_all_tasks_status(self, sandbox):
        """Launch 2 bg tasks, check both show up, wait for short one, cancel long one."""
        agent = create_eval_agent(
            system_prompt=AGENT_PROMPT,
            sandbox_id=sandbox["id"],
            enable_background_tasks=True,
        )
        result = await agent.invoke(
            "Launch 'sleep 3 && echo TASK_A_RESULT' in background (background: true) AND "
            "'sleep 30 && echo TASK_B_RESULT' in background (background: true). "
            "Do 'echo FG' in foreground. "
            "Check progress with check_background_progress — you should see 2 tasks. "
            "Wait for any task with wait_for_background_task timeout=10. "
            "Check progress again — one should be completed, one still running. "
            "Cancel the running one with cancel_background_task. "
            "Report status of both tasks."
        )
        log_result(result,"multi_tasks")

        bg_bash = [tc for tc in result.tool_calls
                   if tc.name == "daytona_shell" and tc.input.get("background") is True]
        assert len(bg_bash) >= 2, \
            f"Expected 2+ background launches. Got {len(bg_bash)}: {result.tool_names}"
        checks = result.tool_count("check_background_progress")
        assert checks >= 2, \
            f"Expected 2+ check_background_progress calls. Got {checks}: {result.tool_names}"
        assert result.has_tool("wait_for_background_task"), \
            f"Expected wait_for_background_task. Got: {result.tool_names}"
        assert result.has_tool("cancel_background_task"), \
            f"Expected cancel_background_task. Got: {result.tool_names}"
        text_lower = result.text.lower()
        assert any(word in text_lower for word in ["complet", "done", "result"]), \
            f"Expected text to mention completed task. Got: {result.text[:300]}"
        assert any(word in text_lower for word in ["cancel", "running", "stop"]), \
            f"Expected text to mention cancelled/running task. Got: {result.text[:300]}"
        assert not result.has_non_cancel_errors, \
            f"Unexpected errors: {[e.output[:200] for e in result.non_cancel_error_events]}"


# ===========================================================================
# Test 5: check_background_progress filtered by task_id
# ===========================================================================


@pytest.mark.skipif(not EvalAgent.has_all(), reason="API + Daytona both required")
class TestCheckProgressFilterByTaskId:
    """LLM uses task_id parameter on check_background_progress for targeted queries."""

    @pytest.fixture(scope="class")
    def sandbox(self):
        sb = create_test_sandbox("prog-taskid")
        yield sb
        delete_test_sandbox(sb["id"])

    @pytest.mark.asyncio
    async def test_check_specific_task_by_id(self, sandbox):
        """Launch 2 tasks, do a full check, then check only the completed task by its id."""
        agent = create_eval_agent(
            system_prompt=AGENT_PROMPT,
            sandbox_id=sandbox["id"],
            enable_background_tasks=True,
        )
        result = await agent.invoke(
            "Launch 'sleep 3 && echo ALPHA_DONE' in background (background: true) AND "
            "'sleep 30 && echo BETA_DONE' in background (background: true). "
            "Do 'echo PREP' in foreground. "
            "Check progress for all tasks first (no task_id). "
            "Then wait for the short task with wait_for_background_task timeout=10. "
            "After it completes, check progress for just the completed task using its task_id. "
            "Cancel the other task with cancel_background_task. "
            "Report what you saw."
        )
        log_result(result,"filter_by_taskid")

        bg_bash = [tc for tc in result.tool_calls
                   if tc.name == "daytona_shell" and tc.input.get("background") is True]
        assert len(bg_bash) >= 2, \
            f"Expected 2+ background launches. Got {len(bg_bash)}: {result.tool_names}"
        checks = result.tool_count("check_background_progress")
        assert checks >= 2, \
            f"Expected 2+ check_background_progress calls. Got {checks}: {result.tool_names}"
        check_calls = [tc for tc in result.tool_calls if tc.name == "check_background_progress"]
        assert any("task_id" in tc.input for tc in check_calls), \
            f"Expected at least one check_background_progress call with task_id. " \
            f"Got inputs: {[tc.input for tc in check_calls]}"
        assert result.has_tool("cancel_background_task"), \
            f"Expected cancel_background_task. Got: {result.tool_names}"
        assert not result.has_non_cancel_errors, \
            f"Unexpected errors: {[e.output[:200] for e in result.non_cancel_error_events]}"


# ===========================================================================
# Test 6: wait_for_background_task with last_n_lines truncation
# ===========================================================================


@pytest.mark.skipif(not EvalAgent.has_all(), reason="API + Daytona both required")
class TestWaitLastNLinesOutput:
    """LLM uses last_n_lines parameter on wait_for_background_task."""

    @pytest.fixture(scope="class")
    def sandbox(self):
        sb = create_test_sandbox("wait-lastn")
        yield sb
        delete_test_sandbox(sb["id"])

    @pytest.mark.asyncio
    async def test_wait_returns_truncated_output(self, sandbox):
        """Launch a task producing 100 lines, wait with last_n_lines=10, report lines seen."""
        agent = create_eval_agent(
            system_prompt=AGENT_PROMPT,
            sandbox_id=sandbox["id"],
            enable_background_tasks=True,
        )
        result = await agent.invoke(
            "Launch 'for i in $(seq 1 100); do echo \"BUILD_LOG_$i\"; done' in background (background: true). "
            "Do 'echo PREP' in foreground. "
            "Check progress with check_background_progress. "
            "Then call wait_for_background_task with timeout=10 and last_n_lines=10. "
            "Report how many output lines you received."
        )
        log_result(result,"wait_last_n")

        assert result.has_tool("wait_for_background_task"), \
            f"Expected wait_for_background_task. Got: {result.tool_names}"
        wait_calls = [tc for tc in result.tool_calls if tc.name == "wait_for_background_task"]
        assert any("last_n_lines" in tc.input for tc in wait_calls), \
            f"Expected wait_for_background_task call with last_n_lines. " \
            f"Got inputs: {[tc.input for tc in wait_calls]}"
        text_lower = result.text.lower()
        assert any(word in text_lower for word in ["build_log", "lines", "output"]), \
            f"Expected text to mention BUILD_LOG/lines/output. Got: {result.text[:300]}"
        assert not result.has_non_cancel_errors, \
            f"Unexpected errors: {[e.output[:200] for e in result.non_cancel_error_events]}"


# ===========================================================================
# Test 7: Live tail drives autonomous early-cancel decision
# ===========================================================================


@pytest.mark.skipif(not EvalAgent.has_all(), reason="API + Daytona both required")
class TestLiveTailAutonomousDecision:
    """The agent reads streamed lines from a still-running task and decides on
    its own to cancel as soon as a FATAL line appears, instead of waiting for
    the long-running task to finish naturally."""

    @pytest.fixture(scope="class")
    def sandbox(self):
        sb = create_test_sandbox("livetail-decide")
        yield sb
        delete_test_sandbox(sb["id"])

    @pytest.mark.asyncio
    async def test_agent_cancels_on_fatal_line_in_live_tail(self, sandbox):
        """A bg task prints normal lines for a while, then a FATAL line, then
        would keep running for another ~30s. The agent must observe the FATAL
        line via check_background_progress and cancel early — NOT wait for the
        full ~45s runtime."""
        agent = create_eval_agent(
            system_prompt=AGENT_PROMPT,
            sandbox_id=sandbox["id"],
            enable_background_tasks=True,
        )
        result = await agent.invoke(
            "Launch this exact bash command in BACKGROUND (background: true):\n\n"
            "  for i in $(seq 1 5); do echo \"step_$i ok\"; sleep 2; done; "
            "echo 'FATAL: disk full'; for i in $(seq 6 20); do echo \"step_$i ok\"; sleep 2; done\n\n"
            "Your job: poll check_background_progress periodically (insert short "
            "'sleep 3' foreground daytona_shell calls between polls). As soon as you "
            "see a line containing the word FATAL in the live tail, IMMEDIATELY "
            "cancel the task with cancel_background_task and stop. Do NOT call "
            "wait_for_background_task — that would block for the full ~45s. "
            "Report the offending line and how many polls it took."
        )
        log_result(result,"live_tail_decision")

        # The agent must have polled check_background_progress at least once
        # and cancelled — never waited.
        assert result.has_tool("check_background_progress"), \
            f"Agent never polled progress. Tools used: {result.tool_names}"
        assert result.has_tool("cancel_background_task"), \
            f"Agent never cancelled. Tools used: {result.tool_names}"
        assert not result.has_tool("wait_for_background_task"), (
            f"Agent waited instead of cancelling on FATAL line. "
            f"Tools used: {result.tool_names}"
        )

        # At least one mid-flight check must have surfaced the FATAL line
        # while the task was still running.
        check_completions = [
            e for e in result.tools_completed() if e.tool_name == "check_background_progress"
        ]
        saw_fatal_in_tail = any(
            '"status": "running"' in (e.output or "") and "FATAL" in (e.output or "")
            for e in check_completions
        )
        assert saw_fatal_in_tail, (
            "No mid-flight check_background_progress call surfaced the FATAL line. "
            f"Outputs: {[(e.output or '')[:300] for e in check_completions]}"
        )

        # The whole run must have been short enough to prove early-cancel:
        # the underlying command would take ~45s if it ran to completion.
        assert result.latency_ms < 40_000, (
            f"Agent took {result.latency_ms:.0f}ms — likely waited for the bg task "
            f"to finish instead of cancelling on the live tail."
        )

        text_lower = result.text.lower()
        assert "fatal" in text_lower, \
            f"Expected agent to report the FATAL line. Got: {result.text[:300]}"
        assert not result.has_non_cancel_errors, \
            f"Unexpected errors: {[e.output[:200] for e in result.non_cancel_error_events]}"
