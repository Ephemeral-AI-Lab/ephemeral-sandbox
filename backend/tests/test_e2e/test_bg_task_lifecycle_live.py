# ruff: noqa
"""Live E2E: Task lifecycle management — progress checks, cancellation, notifications.

Tests thorough usage of check_background_progress, cancel_background_task,
and background task completion notifications across various scenarios.

Run with: .venv/bin/python -m pytest backend/tests/test_e2e/test_bg_task_lifecycle_live.py -v -s --log-cli-level=INFO
"""

from __future__ import annotations

import pytest

from engine.testing.eval_agent import EvalAgent
from tests.test_e2e.bg_prompts import _build_prompt
from tests.test_e2e.conftest import create_eval_agent, create_test_sandbox, delete_test_sandbox
from tests.test_e2e.helpers import assert_fg_during_bg, log_result

pytestmark = [pytest.mark.e2e, pytest.mark.live]

AGENT_PROMPT = _build_prompt(agent_name="test-lifecycle-agent")


# ===========================================================================
# Test 1: Multiple progress checks on a single background task
# ===========================================================================


@pytest.mark.skipif(not EvalAgent.has_all(), reason="API + Daytona both required")
class TestRepeatedProgressChecks:
    """LLM checks progress multiple times on a running background task."""

    @pytest.fixture(scope="class")
    def sandbox(self):
        sb = create_test_sandbox("lc-repeated")
        yield sb
        delete_test_sandbox(sb["id"])

    @pytest.mark.asyncio
    async def test_multiple_progress_checks_before_cancel(self, sandbox):
        """Background a long task, check progress 3+ times, then cancel."""
        agent = create_eval_agent(
            system_prompt=AGENT_PROMPT,
            sandbox_id=sandbox["id"],
            enable_background_tasks=True,
        )
        result = await agent.invoke(
            "Do these steps:\n"
            "1. Run 'sleep 60 && echo LONG_BUILD_DONE' in background (background: true)\n"
            "2. Run 'echo WORK_A' in foreground\n"
            "3. Call check_background_progress to check the background task\n"
            "4. Run 'echo WORK_B' in foreground\n"
            "5. Call check_background_progress again\n"
            "6. Run 'echo WORK_C' in foreground\n"
            "7. Call check_background_progress a third time\n"
            "8. The build is taking too long — cancel it with cancel_background_task "
            "using reason: 'Build exceeded time budget'\n"
            "9. Report the status from each progress check\n\n"
            "You MUST call check_background_progress THREE times (steps 3, 5, 7). "
            "Use background: true for step 1 ONLY."
        )
        log_result(result, "repeated_checks")

        # Strict: background task must use background: true
        assert result.has_tool_with_background("daytona_shell"), \
            f"Expected daytona_shell with background: true. Got: {result.tool_calls}"
        assert len(result.background_started()) >= 1, \
            f"Expected BackgroundTaskStarted event. Got: {result.tool_names}"
        # Strict: at least 3 progress checks (steps 3, 5, 7)
        checks = result.tool_count("check_background_progress")
        assert checks >= 3, \
            f"Expected 3+ progress checks (requested 3). Got {checks}: {result.tool_names}"
        # Strict: 3 foreground bash calls interspersed with checks
        fg_bash = [tc for tc in result.tool_calls
                   if tc.name == "daytona_shell" and not tc.input.get("background")]
        assert len(fg_bash) >= 3, \
            f"Expected 3+ foreground bash calls (WORK_A/B/C). Got {len(fg_bash)}"
        # Strict: cancel must happen AFTER progress checks
        check_indices = [i for i, tc in enumerate(result.tool_calls)
                         if tc.name == "check_background_progress"]
        cancel_indices = [i for i, tc in enumerate(result.tool_calls)
                          if tc.name == "cancel_background_task"]
        assert cancel_indices, f"Expected cancel_background_task. Got: {result.tool_names}"
        assert cancel_indices[0] > check_indices[0], \
            f"Cancel must happen after progress checks. checks={check_indices}, cancels={cancel_indices}"
        # Strict: fg work must happen WHILE bg task is running (true concurrency)
        assert_fg_during_bg(result, min_fg=1)
        assert not result.has_non_cancel_errors, \
            f"Unexpected errors: {[e.output[:200] for e in result.non_cancel_error_events]}"


# ===========================================================================
# Test 2: Selective cancellation — cancel one, keep another
# ===========================================================================


@pytest.mark.skipif(not EvalAgent.has_all(), reason="API + Daytona both required")
class TestSelectiveCancellation:
    """Cancel one background task while keeping another running."""

    @pytest.fixture(scope="class")
    def sandbox(self):
        sb = create_test_sandbox("lc-selective")
        yield sb
        delete_test_sandbox(sb["id"])

    @pytest.mark.asyncio
    async def test_cancel_one_keep_one(self, sandbox):
        """Launch 2 bg tasks, cancel the slow one, keep the fast one."""
        agent = create_eval_agent(
            system_prompt=AGENT_PROMPT,
            sandbox_id=sandbox["id"],
            enable_background_tasks=True,
        )
        result = await agent.invoke(
            "Do these steps:\n"
            "1. Run 'sleep 8 && echo FAST_TEST_DONE' in background (background: true) — this is the fast test\n"
            "2. Run 'sleep 120 && echo SLOW_DEPLOY_DONE' in background (background: true) — this is the slow deploy\n"
            "3. Run 'echo WORKING_ON_FIX' in foreground\n"
            "4. Check background progress using check_background_progress\n"
            "5. Cancel ONLY the slow deploy task (the one with 'sleep 120') using cancel_background_task "
            "with reason: 'Deploy is taking too long, cancelling'\n"
            "6. Check progress again to confirm the fast test is still running or completed\n"
            "7. Report which tasks are still active\n\n"
            "Use background: true for steps 1 and 2 ONLY."
        )
        log_result(result, "selective_cancel")

        # Strict: exactly 2 background launches with background: true
        bg_bash = [tc for tc in result.tool_calls
                   if tc.name == "daytona_shell" and tc.input.get("background") is True]
        assert len(bg_bash) >= 2, \
            f"Expected 2+ daytona_shell with background: true. Got {len(bg_bash)}"
        assert len(result.background_started()) >= 2, \
            f"Expected 2 BackgroundTaskStarted events. Got {len(result.background_started())}"
        # Strict: cancel must provide a reason
        cancel_calls = [tc for tc in result.tool_calls if tc.name == "cancel_background_task"]
        assert len(cancel_calls) >= 1, \
            f"Expected cancel_background_task. Got: {result.tool_names}"
        assert any(tc.input.get("reason") for tc in cancel_calls), \
            f"Expected cancel with a reason. Got: {[tc.input for tc in cancel_calls]}"
        # Strict: 2+ progress checks (before and after cancel)
        checks = result.tool_count("check_background_progress")
        assert checks >= 2, \
            f"Expected 2+ progress checks (before + after cancel). Got {checks}"
        # Strict: foreground work happened
        fg_bash = [tc for tc in result.tool_calls
                   if tc.name == "daytona_shell" and not tc.input.get("background")]
        assert len(fg_bash) >= 1, \
            f"Expected foreground bash call. Got {len(fg_bash)}"
        # Strict: fg work must happen WHILE bg tasks are running (true concurrency)
        assert_fg_during_bg(result, min_fg=1)
        assert not result.has_non_cancel_errors, \
            f"Unexpected errors: {[e.output[:200] for e in result.non_cancel_error_events]}"


# ===========================================================================
# Test 3: Cancel all background tasks in batch
# ===========================================================================


@pytest.mark.skipif(not EvalAgent.has_all(), reason="API + Daytona both required")
class TestBatchCancellation:
    """Cancel multiple background tasks one after another."""

    @pytest.fixture(scope="class")
    def sandbox(self):
        sb = create_test_sandbox("lc-batch-cancel")
        yield sb
        delete_test_sandbox(sb["id"])

    @pytest.mark.asyncio
    async def test_cancel_three_tasks_sequentially(self, sandbox):
        """Launch 3 bg tasks, check progress, then cancel each one."""
        agent = create_eval_agent(
            system_prompt=AGENT_PROMPT,
            sandbox_id=sandbox["id"],
            enable_background_tasks=True,
        )
        result = await agent.invoke(
            "Execute these steps:\n"
            "1. Run 'sleep 30 && echo TASK_A' in background (background: true)\n"
            "2. Run 'sleep 40 && echo TASK_B' in background (background: true)\n"
            "3. Run 'sleep 50 && echo TASK_C' in background (background: true)\n"
            "4. Run 'echo FG_DONE' in foreground\n"
            "5. Check progress of all tasks using check_background_progress\n"
            "6. Cancel ALL three background tasks one by one using cancel_background_task. "
            "Use reason: 'Batch cleanup — cancelling all pending tasks' for each.\n"
            "7. Check progress again to confirm all cancelled\n"
            "8. Report final state\n\n"
            "Use background: true for steps 1-3 ONLY."
        )
        log_result(result, "batch_cancel")

        # Strict: 3 background launches with background: true
        bg_bash = [tc for tc in result.tool_calls
                   if tc.name == "daytona_shell" and tc.input.get("background") is True]
        assert len(bg_bash) >= 3, \
            f"Expected 3+ daytona_shell with background: true. Got {len(bg_bash)}"
        assert len(result.background_started()) >= 3, \
            f"Expected 3 BackgroundTaskStarted events. Got {len(result.background_started())}"
        # Strict: 3 cancellations (one per task)
        cancels = result.tool_count("cancel_background_task")
        assert cancels >= 3, \
            f"Expected 3+ cancellations (one per bg task). Got {cancels}: {result.tool_names}"
        # Strict: each cancel must have a reason
        cancel_calls = [tc for tc in result.tool_calls if tc.name == "cancel_background_task"]
        for i, cc in enumerate(cancel_calls):
            assert cc.input.get("reason"), \
                f"Cancel call #{i+1} missing reason. Got: {cc.input}"
        # Strict: 2+ progress checks (before + after cancels)
        checks = result.tool_count("check_background_progress")
        assert checks >= 2, \
            f"Expected 2+ progress checks (before + after cancels). Got {checks}"
        # Strict: foreground work happened
        fg_bash = [tc for tc in result.tool_calls
                   if tc.name == "daytona_shell" and not tc.input.get("background")]
        assert len(fg_bash) >= 1, \
            f"Expected foreground bash call. Got {len(fg_bash)}"
        assert not result.has_non_cancel_errors, \
            f"Unexpected errors: {[e.output[:200] for e in result.non_cancel_error_events]}"


# ===========================================================================
# Test 4: Background task notification — short task completes naturally
# ===========================================================================


@pytest.mark.skipif(not EvalAgent.has_all(), reason="API + Daytona both required")
class TestBackgroundCompletion:
    """Background task completes while LLM does foreground work."""

    @pytest.fixture(scope="class")
    def sandbox(self):
        sb = create_test_sandbox("lc-notify")
        yield sb
        delete_test_sandbox(sb["id"])

    @pytest.mark.asyncio
    async def test_bg_task_completes_during_fg_work(self, sandbox):
        """Short bg task should complete while LLM does enough fg work."""
        agent = create_eval_agent(
            system_prompt=AGENT_PROMPT,
            sandbox_id=sandbox["id"],
            enable_background_tasks=True,
        )
        result = await agent.invoke(
            "Do these steps:\n"
            "1. Run 'sleep 3 && echo SHORT_BG_DONE' in background (background: true)\n"
            "2. Run 'echo FG_1' in foreground\n"
            "3. Run 'echo FG_2' in foreground\n"
            "4. Run 'echo FG_3' in foreground\n"
            "5. Run 'echo FG_4' in foreground\n"
            "6. Run 'echo FG_5' in foreground\n"
            "7. Call check_background_progress to check on the background task — it should be done by now\n"
            "8. Report whether the background task completed successfully\n\n"
            "Use background: true for step 1 ONLY."
        )
        log_result(result, "bg_completion")

        # Strict: background task must use background: true
        assert result.has_tool_with_background("daytona_shell"), \
            f"Expected daytona_shell with background: true. Got: {result.tool_calls}"
        assert len(result.background_started()) >= 1, \
            f"Expected BackgroundTaskStarted event. Got: {result.tool_names}"
        # Strict: 5 foreground bash calls (FG_1 through FG_5)
        fg_bash = [tc for tc in result.tool_calls
                   if tc.name == "daytona_shell" and not tc.input.get("background")]
        assert len(fg_bash) >= 5, \
            f"Expected 5+ foreground bash calls (FG_1-FG_5). Got {len(fg_bash)}"
        assert result.has_tool("check_background_progress"), \
            f"Expected progress check. Got: {result.tool_names}"
        # Background launches appear as BackgroundTaskStarted, not ToolExecutionStarted
        # Foreground total: 5 fg bash + 1 check = 6
        assert len(result.tools_started()) >= 6, \
            f"Expected 6+ foreground tool calls. Got {len(result.tools_started())}"
        assert len(result.tools_started()) + len(result.background_started()) >= 7, \
            f"Expected 7+ total actions (fg + bg). Got {len(result.tools_started())} fg + {len(result.background_started())} bg"
        # Strict: the short bg task (3s) should complete — check for BackgroundTaskCompleted
        # or the LLM text acknowledging completion
        bg_completed = result.background_completed()
        text_mentions_done = any(
            word in result.text.lower()
            for word in ["completed", "done", "finished", "success", "short_bg_done"]
        )
        assert len(bg_completed) >= 1 or text_mentions_done, \
            f"Expected background task to complete (3s sleep). bg_completed={len(bg_completed)}, text={result.text[:200]}"
        # Strict: fg work must happen WHILE bg task is running (true concurrency)
        assert_fg_during_bg(result, min_fg=3)
        assert not result.has_non_cancel_errors, \
            f"Unexpected errors: {[e.output[:200] for e in result.non_cancel_error_events]}"


# ===========================================================================
# Test 5: Progress check reveals error — LLM reacts appropriately
# ===========================================================================


@pytest.mark.skipif(not EvalAgent.has_all(), reason="API + Daytona both required")
class TestProgressCheckRevealsError:
    """Background task that fails — LLM should detect and handle the error."""

    @pytest.fixture(scope="class")
    def sandbox(self):
        sb = create_test_sandbox("lc-error")
        yield sb
        delete_test_sandbox(sb["id"])

    @pytest.mark.asyncio
    async def test_bg_failure_detected_on_progress_check(self, sandbox):
        """Run a bg task that fails fast, check progress, react to the error."""
        agent = create_eval_agent(
            system_prompt=AGENT_PROMPT,
            sandbox_id=sandbox["id"],
            enable_background_tasks=True,
        )
        result = await agent.invoke(
            "Do these steps:\n"
            "1. Run 'sleep 2 && exit 1' in background (background: true) — "
            "this simulates a failing test suite\n"
            "2. Run 'echo DOING_OTHER_WORK' in foreground\n"
            "3. Run 'echo MORE_WORK' in foreground\n"
            "4. Check background progress using check_background_progress\n"
            "5. Based on what you see, report whether the background task succeeded or failed\n"
            "6. If the task failed, explain what happened\n\n"
            "Use background: true for step 1 ONLY."
        )
        log_result(result, "bg_error")

        # Strict: background task must use background: true
        assert result.has_tool_with_background("daytona_shell"), \
            f"Expected daytona_shell with background: true. Got: {result.tool_calls}"
        assert len(result.background_started()) >= 1, \
            f"Expected BackgroundTaskStarted event. Got: {result.tool_names}"
        # Strict: 2 foreground bash calls
        fg_bash = [tc for tc in result.tool_calls
                   if tc.name == "daytona_shell" and not tc.input.get("background")]
        assert len(fg_bash) >= 2, \
            f"Expected 2+ foreground bash calls. Got {len(fg_bash)}"
        assert result.has_tool("check_background_progress"), \
            f"Expected progress check. Got: {result.tool_names}"
        # Strict: LLM must acknowledge the failure in its response text
        assert len(result.assistant_turns()) >= 1, \
            "Expected assistant to report on the error"
        text_lower = result.text.lower()
        assert any(word in text_lower for word in ["fail", "error", "exit", "non-zero"]), \
            f"Expected LLM to mention failure/error in report. Got: {result.text[:300]}"
