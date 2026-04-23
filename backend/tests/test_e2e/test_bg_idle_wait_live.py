# ruff: noqa
"""Live E2E: Idle and wait scenarios — waiting for background tasks to complete.

Tests that the LLM correctly handles idle periods while waiting for
background tasks, including polling, waiting, and reacting to completion.

Run with: .venv/bin/python -m pytest backend/tests/test_e2e/test_bg_idle_wait_live.py -v -s --log-cli-level=INFO
"""

from __future__ import annotations

import pytest

from engine.testing.eval_agent import EvalAgent
from tests.test_e2e.bg_prompts import BG_IDLE_WAIT
from tests.test_e2e.conftest import create_eval_agent, create_test_sandbox, delete_test_sandbox
from tests.test_e2e.helpers import assert_fg_during_bg, log_result

pytestmark = [pytest.mark.e2e, pytest.mark.live]

AGENT_PROMPT = BG_IDLE_WAIT


# ===========================================================================
# Test 1: Wait for a short background task to complete
# ===========================================================================


@pytest.mark.skipif(not EvalAgent.has_all(), reason="API + Daytona both required")
class TestWaitForShortTask:
    """LLM waits and polls until a short background task finishes."""

    @pytest.fixture(scope="class")
    def sandbox(self):
        sb = create_test_sandbox("idle-short")
        yield sb
        delete_test_sandbox(sb["id"])

    @pytest.mark.asyncio
    async def test_poll_until_short_task_completes(self, sandbox):
        """Background a 5s task, do minimal fg work, keep checking until done."""
        agent = create_eval_agent(
            system_prompt=AGENT_PROMPT,
            sandbox_id=sandbox["id"],
            enable_background_tasks=True,
        )
        result = await agent.invoke(
            "Do these steps:\n"
            "1. Run 'sleep 5 && echo QUICK_BUILD_DONE' in background (background: true)\n"
            "2. Run 'echo PREP_DONE' in foreground\n"
            "3. Now wait for the background task to finish. Keep checking progress "
            "using check_background_progress until it shows as completed.\n"
            "4. Once the task is done, report the final output.\n\n"
            "Use background: true for step 1 ONLY. "
            "You MUST call check_background_progress at least once."
        )
        log_result(result, "wait_short")

        # Strict: background task must use background: true
        assert result.has_tool_with_background("daytona_shell"), \
            f"Expected daytona_shell with background: true. Got: {result.tool_calls}"
        assert len(result.background_started()) >= 1, \
            f"Expected BackgroundTaskStarted event. Got: {result.tool_names}"
        # Strict: foreground prep work happened
        fg_bash = [tc for tc in result.tool_calls
                   if tc.name == "daytona_shell" and not tc.input.get("background")]
        assert len(fg_bash) >= 1, \
            f"Expected 1+ foreground bash call. Got {len(fg_bash)}"
        # Strict: at least 1 progress check
        checks = result.tool_count("check_background_progress")
        assert checks >= 1, \
            f"Expected 1+ progress checks while waiting. Got {checks}"
        # Strict: LLM must report completion or output in final text
        assert len(result.assistant_turns()) >= 1, "Missing final report"
        text_lower = result.text.lower()
        assert any(word in text_lower for word in ["done", "complet", "quick_build_done", "finish", "success"]), \
            f"Expected LLM to report task outcome. Got: {result.text[:300]}"
        # Strict: fg prep must happen WHILE bg task is running (true concurrency)
        assert_fg_during_bg(result, min_fg=1)
        assert not result.has_non_cancel_errors, \
            f"Unexpected errors: {[e.output[:200] for e in result.non_cancel_error_events]}"


# ===========================================================================
# Test 2: Foreground work exhausted — idle polling on background
# ===========================================================================


@pytest.mark.skipif(not EvalAgent.has_all(), reason="API + Daytona both required")
class TestIdleAfterForegroundExhausted:
    """All foreground work done, LLM enters idle polling mode."""

    @pytest.fixture(scope="class")
    def sandbox(self):
        sb = create_test_sandbox("idle-exhausted")
        yield sb
        delete_test_sandbox(sb["id"])

    @pytest.mark.asyncio
    async def test_idle_polling_after_all_fg_done(self, sandbox):
        """Finish all fg work quickly, then poll bg task."""
        agent = create_eval_agent(
            system_prompt=AGENT_PROMPT,
            sandbox_id=sandbox["id"],
            enable_background_tasks=True,
        )
        result = await agent.invoke(
            "Do these steps:\n"
            "1. Run 'sleep 20 && echo INTEGRATION_TESTS_DONE' in background (background: true)\n"
            "2. Run 'echo FG_TASK_1' in foreground\n"
            "3. Run 'echo FG_TASK_2' in foreground\n"
            "4. That's all the foreground work. Now you are idle.\n"
            "5. Check background progress using check_background_progress\n"
            "6. The task is still running — check again\n"
            "7. It's still running and taking too long — cancel it with "
            "cancel_background_task using reason: 'Timed out waiting'\n"
            "8. Report what happened\n\n"
            "Use background: true for step 1 ONLY."
        )
        log_result(result, "idle_exhausted")

        # Strict: background task must use background: true
        assert result.has_tool_with_background("daytona_shell"), \
            f"Expected daytona_shell with background: true. Got: {result.tool_calls}"
        assert len(result.background_started()) >= 1, \
            f"Expected BackgroundTaskStarted event. Got: {result.tool_names}"
        # Strict: 2 foreground bash calls completed before idle phase
        fg_bash = [tc for tc in result.tool_calls
                   if tc.name == "daytona_shell" and not tc.input.get("background")]
        assert len(fg_bash) >= 2, \
            f"Expected 2+ foreground bash calls (FG_TASK_1/2). Got {len(fg_bash)}"
        # Strict: 2+ progress checks during idle phase
        checks = result.tool_count("check_background_progress")
        assert checks >= 2, \
            f"Expected 2+ idle progress checks (steps 5+6). Got {checks}"
        # Strict: cancel must happen AFTER progress checks
        cancel_calls = [tc for tc in result.tool_calls if tc.name == "cancel_background_task"]
        assert len(cancel_calls) >= 1, \
            f"Expected cancel after idle timeout. Got: {result.tool_names}"
        assert cancel_calls[0].input.get("reason"), \
            f"Expected cancel with reason. Got: {cancel_calls[0].input}"
        # Strict: cancel after checks in tool order
        check_indices = [i for i, tc in enumerate(result.tool_calls)
                         if tc.name == "check_background_progress"]
        cancel_indices = [i for i, tc in enumerate(result.tool_calls)
                          if tc.name == "cancel_background_task"]
        assert cancel_indices[0] > check_indices[0], \
            f"Cancel must happen after progress checks. checks={check_indices}, cancels={cancel_indices}"
        # Strict: fg work must happen WHILE bg task is running (true concurrency)
        assert_fg_during_bg(result, min_fg=2)
        assert not result.has_non_cancel_errors, \
            f"Unexpected errors: {[e.output[:200] for e in result.non_cancel_error_events]}"


# ===========================================================================
# Test 3: Wait for two background tasks — staggered completion
# ===========================================================================


@pytest.mark.skipif(not EvalAgent.has_all(), reason="API + Daytona both required")
class TestStaggeredCompletion:
    """Two bg tasks with different durations — fast one finishes first."""

    @pytest.fixture(scope="class")
    def sandbox(self):
        sb = create_test_sandbox("idle-stagger")
        yield sb
        delete_test_sandbox(sb["id"])

    @pytest.mark.asyncio
    async def test_wait_staggered_bg_tasks(self, sandbox):
        """Launch fast + slow bg tasks, poll, see fast finish, cancel slow."""
        agent = create_eval_agent(
            system_prompt=AGENT_PROMPT,
            sandbox_id=sandbox["id"],
            enable_background_tasks=True,
        )
        result = await agent.invoke(
            "Do these steps:\n"
            "1. Run 'sleep 5 && echo FAST_DONE' in background (background: true)\n"
            "2. Run 'sleep 60 && echo SLOW_DONE' in background (background: true)\n"
            "3. Run 'echo PREP_COMPLETE' in foreground\n"
            "4. Check background progress — one might be done already\n"
            "5. Check background progress again\n"
            "6. The slow task is still running — cancel it with cancel_background_task "
            "using reason: 'Slow task not needed anymore'\n"
            "7. Report: which task finished and which was cancelled?\n\n"
            "Use background: true for steps 1-2 ONLY."
        )
        log_result(result, "staggered")

        # Strict: 2 background launches with background: true
        bg_bash = [tc for tc in result.tool_calls
                   if tc.name == "daytona_shell" and tc.input.get("background") is True]
        assert len(bg_bash) >= 2, \
            f"Expected 2+ daytona_shell with background: true. Got {len(bg_bash)}"
        assert len(result.background_started()) >= 2, \
            f"Expected 2 BackgroundTaskStarted events. Got {len(result.background_started())}"
        # Strict: 2+ progress checks
        checks = result.tool_count("check_background_progress")
        assert checks >= 2, \
            f"Expected 2+ progress checks. Got {checks}"
        # Strict: cancel with reason
        cancel_calls = [tc for tc in result.tool_calls if tc.name == "cancel_background_task"]
        assert len(cancel_calls) >= 1, \
            f"Expected cancel of slow task. Got: {result.tool_names}"
        assert cancel_calls[0].input.get("reason"), \
            f"Expected cancel with reason. Got: {cancel_calls[0].input}"
        # Strict: LLM text must distinguish between fast (finished) and slow (cancelled)
        text_lower = result.text.lower()
        assert any(w in text_lower for w in ["fast", "finish", "complet", "done", "fast_done"]), \
            f"Expected LLM to mention fast task completing. Got: {result.text[:300]}"
        assert any(w in text_lower for w in ["cancel", "slow", "stop"]), \
            f"Expected LLM to mention slow task cancellation. Got: {result.text[:300]}"
        # Strict: fg prep must happen WHILE bg tasks are running (true concurrency)
        assert_fg_during_bg(result, min_fg=1)
        assert not result.has_non_cancel_errors, \
            f"Unexpected errors: {[e.output[:200] for e in result.non_cancel_error_events]}"


# ===========================================================================
# Test 4: Idle with no foreground work — pure background monitoring
# ===========================================================================


@pytest.mark.skipif(not EvalAgent.has_all(), reason="API + Daytona both required")
class TestPureBackgroundMonitoring:
    """No foreground tasks — LLM's only job is to monitor background tasks."""

    @pytest.fixture(scope="class")
    def sandbox(self):
        sb = create_test_sandbox("idle-pure")
        yield sb
        delete_test_sandbox(sb["id"])

    @pytest.mark.asyncio
    async def test_monitor_only_no_fg_work(self, sandbox):
        """Launch bg tasks, provide NO fg work — LLM must poll and manage."""
        agent = create_eval_agent(
            system_prompt=AGENT_PROMPT,
            sandbox_id=sandbox["id"],
            enable_background_tasks=True,
        )
        result = await agent.invoke(
            "Launch these background tasks and monitor them:\n"
            "1. Run 'sleep 5 && echo MONITOR_A_DONE' in background (background: true)\n"
            "2. Run 'sleep 45 && echo MONITOR_B_DONE' in background (background: true)\n\n"
            "There is NO foreground work to do. Your job is to:\n"
            "- Check progress using check_background_progress\n"
            "- If the first task completes, note it\n"
            "- The second task is too slow — cancel it using cancel_background_task "
            "with reason: 'Monitor timeout exceeded'\n"
            "- Report final status of both tasks\n\n"
            "Use background: true for steps 1-2."
        )
        log_result(result, "pure_monitor")

        # Strict: 2 background launches with background: true
        bg_bash = [tc for tc in result.tool_calls
                   if tc.name == "daytona_shell" and tc.input.get("background") is True]
        assert len(bg_bash) >= 2, \
            f"Expected 2+ daytona_shell with background: true. Got {len(bg_bash)}"
        assert len(result.background_started()) >= 2, \
            f"Expected 2 BackgroundTaskStarted events. Got {len(result.background_started())}"
        # Strict: NO foreground bash calls (pure monitoring, no fg work)
        fg_bash = [tc for tc in result.tool_calls
                   if tc.name == "daytona_shell" and not tc.input.get("background")]
        assert len(fg_bash) == 0, \
            f"Expected NO foreground bash calls (pure monitor mode). Got {len(fg_bash)}: {[tc.input for tc in fg_bash]}"
        # Strict: 1+ progress checks
        checks = result.tool_count("check_background_progress")
        assert checks >= 1, \
            f"Expected 1+ progress checks. Got {checks}"
        # Strict: cancel with reason
        cancel_calls = [tc for tc in result.tool_calls if tc.name == "cancel_background_task"]
        assert len(cancel_calls) >= 1, \
            f"Expected cancel of slow task. Got: {result.tool_names}"
        assert cancel_calls[0].input.get("reason"), \
            f"Expected cancel with reason. Got: {cancel_calls[0].input}"
        # Strict: LLM must report on both tasks
        text_lower = result.text.lower()
        assert any(w in text_lower for w in ["monitor_a", "first", "fast", "complet", "done"]), \
            f"Expected report on first task. Got: {result.text[:300]}"
        assert any(w in text_lower for w in ["cancel", "monitor_b", "second", "slow", "timeout"]), \
            f"Expected report on cancelled task. Got: {result.text[:300]}"


# ===========================================================================
# Test 5: Wait then act — use background result to drive next action
# ===========================================================================


@pytest.mark.skipif(not EvalAgent.has_all(), reason="API + Daytona both required")
class TestWaitThenAct:
    """Wait for bg task, then use its result to decide next action."""

    @pytest.fixture(scope="class")
    def sandbox(self):
        sb = create_test_sandbox("idle-then-act")
        yield sb
        delete_test_sandbox(sb["id"])

    @pytest.mark.asyncio
    async def test_bg_result_drives_next_action(self, sandbox):
        """Background a task, wait for result, then act based on output."""
        agent = create_eval_agent(
            system_prompt=AGENT_PROMPT,
            sandbox_id=sandbox["id"],
            enable_background_tasks=True,
        )
        result = await agent.invoke(
            "Do these steps:\n"
            "1. Run 'sleep 3 && echo BUILD_SUCCESS' in background (background: true)\n"
            "2. Run 'echo PREPARING_DEPLOY' in foreground\n"
            "3. Check background progress using check_background_progress\n"
            "4. If the background build succeeded (output contains 'BUILD_SUCCESS'), "
            "create /home/daytona/deploy_ready.txt with 'READY_TO_DEPLOY' using daytona_write_file\n"
            "5. If the build is still running, check progress again, then create the file\n"
            "6. Run 'cat /home/daytona/deploy_ready.txt' in foreground to verify\n"
            "7. Report the complete workflow\n\n"
            "Use background: true for step 1 ONLY."
        )
        log_result(result, "wait_then_act")

        # Strict: background task with background: true
        assert result.has_tool_with_background("daytona_shell"), \
            f"Expected daytona_shell with background: true. Got: {result.tool_calls}"
        assert len(result.background_started()) >= 1, \
            f"Expected BackgroundTaskStarted event. Got: {result.tool_names}"
        # Strict: progress check happened
        assert result.has_tool("check_background_progress"), \
            f"Expected progress check. Got: {result.tool_names}"
        # Strict: file creation must happen AFTER progress check (depends on bg result)
        write_calls = [tc for tc in result.tool_calls if tc.name == "daytona_write_file"]
        assert len(write_calls) >= 1, \
            f"Expected daytona_write_file. Got: {result.tool_names}"
        assert any("deploy_ready" in tc.input.get("file_path", "") for tc in write_calls), \
            f"Expected deploy_ready.txt write. Got paths: {[tc.input.get('file_path') for tc in write_calls]}"
        # Strict: write must contain correct content
        deploy_write = [tc for tc in write_calls if "deploy_ready" in tc.input.get("file_path", "")]
        if deploy_write:
            assert "READY_TO_DEPLOY" in deploy_write[0].input.get("content", ""), \
                f"Expected 'READY_TO_DEPLOY' content. Got: {deploy_write[0].input}"
        # Strict: ordering — check_progress before write_file
        check_indices = [i for i, tc in enumerate(result.tool_calls)
                         if tc.name == "check_background_progress"]
        write_indices = [i for i, tc in enumerate(result.tool_calls)
                         if tc.name == "daytona_write_file"]
        assert write_indices[0] > check_indices[0], \
            f"File write must happen after progress check. checks={check_indices}, writes={write_indices}"
        # Strict: verification read of the file
        fg_bash = [tc for tc in result.tool_calls
                   if tc.name == "daytona_shell" and not tc.input.get("background")]
        assert any("cat" in str(tc.input) and "deploy_ready" in str(tc.input) for tc in fg_bash), \
            f"Expected 'cat deploy_ready.txt' verification. Got fg calls: {[tc.input for tc in fg_bash]}"
        # Background launches appear as BackgroundTaskStarted, not ToolExecutionStarted
        # Foreground total: 1 fg bash + 1 check + 1 write + 1 cat = 4+
        assert len(result.tools_started()) >= 4, \
            f"Expected 4+ foreground tool calls. Got {len(result.tools_started())}"
        assert len(result.tools_started()) + len(result.background_started()) >= 5, \
            f"Expected 5+ total actions (fg + bg). Got {len(result.tools_started())} fg + {len(result.background_started())} bg"
        # Strict: fg prep must happen WHILE bg task is running (true concurrency)
        assert_fg_during_bg(result, min_fg=1)
        # Use unrecovered: agent may hit transient errors (e.g. cat before
        # write_file flushes) and retry successfully — that's acceptable.
        assert not result.has_unrecovered_errors, \
            f"Unexpected unrecovered errors: {[e.output[:200] for e in result.unrecovered_error_events]}"
