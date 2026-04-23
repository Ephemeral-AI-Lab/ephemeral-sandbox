# ruff: noqa
"""Live E2E: Parallel background/foreground task orchestration.

Tests that the LLM correctly manages multiple concurrent background tasks
alongside foreground work, making intelligent decisions about task ordering,
resource usage, and result aggregation.

Run with: .venv/bin/python -m pytest backend/tests/test_e2e/test_bg_parallel_tasks_live.py -v -s --log-cli-level=INFO
"""
from __future__ import annotations
import pytest
from engine.testing.eval_agent import EvalAgent
from tests.test_e2e.bg_prompts import BG_PARALLEL
from tests.test_e2e.conftest import create_eval_agent, create_test_sandbox, delete_test_sandbox
from tests.test_e2e.helpers import log_result

pytestmark = [pytest.mark.e2e, pytest.mark.live]

AGENT_PROMPT = BG_PARALLEL


# ===========================================================================
# Test 1: Three parallel background tasks with foreground interleaving
# ===========================================================================


@pytest.mark.skipif(not EvalAgent.has_all(), reason="API + Daytona both required")
class TestParallelBgWithFgInterleaving:
    """LLM launches 3 background tasks and interleaves foreground work."""

    @pytest.fixture(scope="class")
    def sandbox(self):
        sb = create_test_sandbox("par-interleave")
        yield sb
        delete_test_sandbox(sb["id"])

    @pytest.mark.asyncio
    async def test_three_bg_tasks_with_fg_work_between(self, sandbox):
        """Launch 3 independent background tasks, do foreground work, wait for all."""
        agent = create_eval_agent(
            system_prompt=AGENT_PROMPT,
            sandbox_id=sandbox["id"],
            enable_background_tasks=True,
        )
        result = await agent.invoke(
            "Launch 3 independent background tasks simultaneously:\n"
            "1. 'sleep 3 && echo BUILD_DONE' (background: true)\n"
            "2. 'sleep 4 && echo TEST_DONE' (background: true)\n"
            "3. 'sleep 5 && echo LINT_DONE' (background: true)\n"
            "While they run, do these foreground tasks:\n"
            "- 'echo \"Step 1: Config loaded\"'\n"
            "- 'echo \"Step 2: Env verified\"'\n"
            "- 'echo \"Step 3: Deps checked\"'\n"
            "Then check progress using check_background_progress. "
            "Wait for all tasks with wait_for_background_task using task_id=\"all\" and timeout=15. "
            "Report all results."
        )
        log_result(result, "three_bg_fg_interleave")

        # 3+ background launches
        bg_bash = [tc for tc in result.tool_calls
                   if tc.name == "daytona_shell" and tc.input.get("background") is True]
        assert len(bg_bash) >= 3, \
            f"Expected 3+ background launches. Got {len(bg_bash)}: {result.tool_names}"
        assert len(result.background_started()) >= 3, \
            f"Expected 3+ BackgroundTaskStarted events. Got {len(result.background_started())}"
        # 3+ foreground bash calls (not background)
        fg_bash = [tc for tc in result.tool_calls
                   if tc.name == "daytona_shell" and not tc.input.get("background")]
        assert len(fg_bash) >= 3, \
            f"Expected 3+ foreground bash calls. Got {len(fg_bash)}: {result.tool_names}"
        # check_background_progress must be called
        assert result.has_tool("check_background_progress"), \
            f"Expected check_background_progress. Got: {result.tool_names}"
        # wait_for_background_task must be called
        assert result.has_tool("wait_for_background_task"), \
            f"Expected wait_for_background_task. Got: {result.tool_names}"
        # wait call must use wait_for_all=True
        wait_calls = [tc for tc in result.tool_calls if tc.name == "wait_for_background_task"]
        assert any(tc.input.get("task_id") == "all" for tc in wait_calls), \
            f"Expected wait_for_background_task with wait_for_all=True. Got: {[tc.input for tc in wait_calls]}"
        # text mentions results or completion
        text_lower = result.text.lower()
        assert any(w in text_lower for w in ["build_done", "test_done", "lint_done", "all", "complet"]), \
            f"Expected text to mention results or completion. Got: {result.text[:300]}"
        assert not result.has_non_cancel_errors, \
            f"Unexpected errors: {[e.output[:200] for e in result.non_cancel_error_events]}"


# ===========================================================================
# Test 2: Staggered background completion with individual handling
# ===========================================================================


@pytest.mark.skipif(not EvalAgent.has_all(), reason="API + Daytona both required")
class TestParallelBgStaggeredFinish:
    """Background tasks with different durations — handle individually, cancel the slow one."""

    @pytest.fixture(scope="class")
    def sandbox(self):
        sb = create_test_sandbox("par-stagger")
        yield sb
        delete_test_sandbox(sb["id"])

    @pytest.mark.asyncio
    async def test_staggered_bg_completion_with_individual_handling(self, sandbox):
        """Launch fast/medium/slow bg tasks, wait individually, cancel the slow one."""
        agent = create_eval_agent(
            system_prompt=AGENT_PROMPT,
            sandbox_id=sandbox["id"],
            enable_background_tasks=True,
        )
        result = await agent.invoke(
            "Launch these background tasks:\n"
            "1. 'sleep 2 && echo FAST_COMPILE_OK' (background: true)\n"
            "2. 'sleep 15 && echo MED_TESTS_OK' (background: true)\n"
            "3. 'sleep 60 && echo SLOW_INTEGRATION' (background: true)\n"
            "Do foreground: 'echo STARTING'. "
            "Check progress using check_background_progress. "
            "Call wait_for_background_task with timeout=6 to wait for the fast task. "
            "Note which task finished first. "
            "Check progress again to see updated statuses. "
            "Call wait_for_background_task again with timeout=20 to wait for the medium task. "
            "After both short tasks finish, cancel the slow integration task with "
            "cancel_background_task using reason 'Integration tests deferred'. "
            "Create /home/daytona/results.txt with a summary of all 3 task outcomes using daytona_write_file. "
            "Report."
        )
        log_result(result, "staggered_finish")

        # 3+ background launches
        bg_bash = [tc for tc in result.tool_calls
                   if tc.name == "daytona_shell" and tc.input.get("background") is True]
        assert len(bg_bash) >= 3, \
            f"Expected 3+ background launches. Got {len(bg_bash)}: {result.tool_names}"
        assert len(result.background_started()) >= 3, \
            f"Expected 3+ BackgroundTaskStarted events. Got {len(result.background_started())}"
        # 2+ wait calls
        waits = result.tool_count("wait_for_background_task")
        assert waits >= 2, \
            f"Expected 2+ wait_for_background_task calls. Got {waits}: {result.tool_names}"
        # 2+ progress checks
        checks = result.tool_count("check_background_progress")
        assert checks >= 2, \
            f"Expected 2+ check_background_progress calls. Got {checks}: {result.tool_names}"
        # cancel must be called
        assert result.has_tool("cancel_background_task"), \
            f"Expected cancel_background_task. Got: {result.tool_names}"
        # file write must target results path
        write_calls = [tc for tc in result.tool_calls if tc.name == "daytona_write_file"]
        assert len(write_calls) >= 1, \
            f"Expected daytona_write_file. Got: {result.tool_names}"
        assert any("results" in tc.input.get("file_path", "") for tc in write_calls), \
            f"Expected daytona_write_file with 'results' in file_path. Got: {[tc.input.get('file_path') for tc in write_calls]}"
        # only the slow task should be cancelled (exactly 1 cancel)
        cancel_count = result.tool_count("cancel_background_task")
        assert cancel_count == 1, \
            f"Expected exactly 1 cancel (for the slow task). Got {cancel_count}: {result.tool_names}"
        assert not result.has_non_cancel_errors, \
            f"Unexpected errors: {[e.output[:200] for e in result.non_cancel_error_events]}"


# ===========================================================================
# Test 3: Heavy foreground work alongside a single background task
# ===========================================================================


@pytest.mark.skipif(not EvalAgent.has_all(), reason="API + Daytona both required")
class TestParallelFgBgMix:
    """LLM does 5 foreground tasks while a background build runs, then waits."""

    @pytest.fixture(scope="class")
    def sandbox(self):
        sb = create_test_sandbox("par-mix")
        yield sb
        delete_test_sandbox(sb["id"])

    @pytest.mark.asyncio
    async def test_heavy_fg_work_alongside_bg(self, sandbox):
        """Single background build + 5 foreground steps, then wait for bg."""
        agent = create_eval_agent(
            system_prompt=AGENT_PROMPT,
            sandbox_id=sandbox["id"],
            enable_background_tasks=True,
        )
        result = await agent.invoke(
            "Launch 'sleep 8 && echo BG_BUILD_COMPLETE' in background (background: true). "
            "While it runs, do 5 foreground tasks:\n"
            "1. 'echo \"FG_1: Creating config\"'\n"
            "2. 'echo \"FG_2: Setting up env\"'\n"
            "3. 'echo \"FG_3: Running quick check\"'\n"
            "4. 'echo \"FG_4: Validating schema\"'\n"
            "5. 'echo \"FG_5: Pre-deploy verify\"'\n"
            "After all foreground work, check progress using check_background_progress. "
            "Then wait for the background task with wait_for_background_task timeout=15. "
            "Report: how many fg tasks completed, bg result."
        )
        log_result(result, "heavy_fg_bg_mix")

        # 1 background launch
        bg_bash = [tc for tc in result.tool_calls
                   if tc.name == "daytona_shell" and tc.input.get("background") is True]
        assert len(bg_bash) >= 1, \
            f"Expected 1+ background launch. Got {len(bg_bash)}: {result.tool_names}"
        assert len(result.background_started()) >= 1, \
            f"Expected BackgroundTaskStarted event. Got {len(result.background_started())}"
        # 5+ foreground bash calls
        fg_bash = [tc for tc in result.tool_calls
                   if tc.name == "daytona_shell" and not tc.input.get("background")]
        assert len(fg_bash) >= 5, \
            f"Expected 5+ foreground bash calls. Got {len(fg_bash)}: {result.tool_names}"
        # check_background_progress must be called
        assert result.has_tool("check_background_progress"), \
            f"Expected check_background_progress. Got: {result.tool_names}"
        # wait_for_background_task must be called
        assert result.has_tool("wait_for_background_task"), \
            f"Expected wait_for_background_task. Got: {result.tool_names}"
        # text mentions bg result or fg completion
        text_lower = result.text.lower()
        assert any(w in text_lower for w in ["bg_build_complete", "complet", "fg_5", "all"]), \
            f"Expected text to mention BG_BUILD_COMPLETE or completion. Got: {result.text[:300]}"
        assert not result.has_non_cancel_errors, \
            f"Unexpected errors: {[e.output[:200] for e in result.non_cancel_error_events]}"


# ===========================================================================
# Test 4: Multiple background tasks with same command, different args (shards)
# ===========================================================================


@pytest.mark.skipif(not EvalAgent.has_all(), reason="API + Daytona both required")
class TestParallelBgSameCommand:
    """Four background test shards launched in parallel, results aggregated."""

    @pytest.fixture(scope="class")
    def sandbox(self):
        sb = create_test_sandbox("par-same")
        yield sb
        delete_test_sandbox(sb["id"])

    @pytest.mark.asyncio
    async def test_multiple_bg_same_command_different_args(self, sandbox):
        """Launch 4 parallel test shards, wait for all, aggregate results into a file."""
        agent = create_eval_agent(
            system_prompt=AGENT_PROMPT,
            sandbox_id=sandbox["id"],
            enable_background_tasks=True,
        )
        result = await agent.invoke(
            "Launch 4 background tasks that simulate parallel test shards:\n"
            "1. 'sleep 2 && echo \"SHARD_1: 10/10 passed\"' (background: true)\n"
            "2. 'sleep 3 && echo \"SHARD_2: 8/10 passed\"' (background: true)\n"
            "3. 'sleep 4 && echo \"SHARD_3: 10/10 passed\"' (background: true)\n"
            "4. 'sleep 5 && echo \"SHARD_4: 9/10 passed\"' (background: true)\n"
            "Do foreground: 'echo TEST_SHARDS_LAUNCHED'. "
            "Check progress using check_background_progress. "
            "Wait for all shards with wait_for_background_task task_id=\"all\" timeout=15. "
            "Collect all results and create /home/daytona/test_summary.txt with the combined "
            "shard results using daytona_write_file. Report total pass/fail."
        )
        log_result(result, "same_command_shards")

        # 4+ background launches
        bg_bash = [tc for tc in result.tool_calls
                   if tc.name == "daytona_shell" and tc.input.get("background") is True]
        assert len(bg_bash) >= 4, \
            f"Expected 4+ background launches (shards). Got {len(bg_bash)}: {result.tool_names}"
        assert len(result.background_started()) >= 4, \
            f"Expected 4+ BackgroundTaskStarted events. Got {len(result.background_started())}"
        # wait_for_all=True
        wait_calls = [tc for tc in result.tool_calls if tc.name == "wait_for_background_task"]
        assert len(wait_calls) >= 1, \
            f"Expected wait_for_background_task. Got: {result.tool_names}"
        assert any(tc.input.get("task_id") == "all" for tc in wait_calls), \
            f"Expected wait_for_background_task with wait_for_all=True. Got: {[tc.input for tc in wait_calls]}"
        # file write with test_summary in path
        write_calls = [tc for tc in result.tool_calls if tc.name == "daytona_write_file"]
        assert len(write_calls) >= 1, \
            f"Expected daytona_write_file. Got: {result.tool_names}"
        assert any("test_summary" in tc.input.get("file_path", "") for tc in write_calls), \
            f"Expected daytona_write_file with 'test_summary' in path. Got: {[tc.input.get('file_path') for tc in write_calls]}"
        # text mentions shards or pass/fail
        text_lower = result.text.lower()
        assert any(w in text_lower for w in ["shard", "passed", "result", "total"]), \
            f"Expected text to mention shards or results. Got: {result.text[:300]}"
        assert not result.has_non_cancel_errors, \
            f"Unexpected errors: {[e.output[:200] for e in result.non_cancel_error_events]}"


# ===========================================================================
# Test 5: One background task fails, others succeed
# ===========================================================================


@pytest.mark.skipif(not EvalAgent.has_all(), reason="API + Daytona both required")
class TestParallelBgOneFailsOthersSucceed:
    """Three background tasks where one fails — LLM should report all outcomes."""

    @pytest.fixture(scope="class")
    def sandbox(self):
        sb = create_test_sandbox("par-partial-fail")
        yield sb
        delete_test_sandbox(sb["id"])

    @pytest.mark.asyncio
    async def test_one_bg_fails_others_succeed(self, sandbox):
        """Two tasks succeed, one fails — agent reports which succeeded and which failed."""
        agent = create_eval_agent(
            system_prompt=AGENT_PROMPT,
            sandbox_id=sandbox["id"],
            enable_background_tasks=True,
        )
        result = await agent.invoke(
            "Launch 3 background tasks:\n"
            "1. 'sleep 2 && echo SUCCESS_A' (background: true)\n"
            "2. 'sleep 2 && exit 1' (background: true) — this one WILL FAIL\n"
            "3. 'sleep 3 && echo SUCCESS_C' (background: true)\n"
            "Do foreground: 'echo MONITORING'. "
            "Check progress using check_background_progress. "
            "Wait for all with wait_for_background_task task_id=\"all\" timeout=10. "
            "Check progress again to see all statuses. "
            "Report: which succeeded, which failed, and note the exit code of the failed task."
        )
        log_result(result, "one_fails_others_succeed")

        # 3 background launches
        bg_bash = [tc for tc in result.tool_calls
                   if tc.name == "daytona_shell" and tc.input.get("background") is True]
        assert len(bg_bash) >= 3, \
            f"Expected 3+ background launches. Got {len(bg_bash)}: {result.tool_names}"
        assert len(result.background_started()) >= 3, \
            f"Expected 3+ BackgroundTaskStarted events. Got {len(result.background_started())}"
        # wait_for_all=True
        wait_calls = [tc for tc in result.tool_calls if tc.name == "wait_for_background_task"]
        assert len(wait_calls) >= 1, \
            f"Expected wait_for_background_task. Got: {result.tool_names}"
        assert any(tc.input.get("task_id") == "all" for tc in wait_calls), \
            f"Expected wait_for_background_task with wait_for_all=True. Got: {[tc.input for tc in wait_calls]}"
        # 1+ progress checks
        checks = result.tool_count("check_background_progress")
        assert checks >= 1, \
            f"Expected 1+ check_background_progress calls. Got {checks}: {result.tool_names}"
        # text mentions failure AND success (agent reports both outcomes)
        text_lower = result.text.lower()
        has_failure_mention = any(w in text_lower for w in ["fail", "error", "exit"])
        has_success_mention = any(w in text_lower for w in ["success", "success_a", "success_c"])
        assert has_failure_mention and has_success_mention, \
            f"Expected text to mention both failure and success. Got: {result.text[:400]}"
        # Note: the failing task IS expected — no assertion on non_cancel_errors


# ===========================================================================
# Test 6: Cancel all remaining tasks after the first one completes (race)
# ===========================================================================


@pytest.mark.skipif(not EvalAgent.has_all(), reason="API + Daytona both required")
class TestParallelBgCancelAllRemaining:
    """Race scenario — first task wins, all remaining slow tasks are cancelled."""

    @pytest.fixture(scope="class")
    def sandbox(self):
        sb = create_test_sandbox("par-cancel-all")
        yield sb
        delete_test_sandbox(sb["id"])

    @pytest.mark.asyncio
    async def test_cancel_all_remaining_after_first_completes(self, sandbox):
        """4 tasks race; first (WINNER) completes, remaining 3 slow ones are cancelled."""
        agent = create_eval_agent(
            system_prompt=AGENT_PROMPT,
            sandbox_id=sandbox["id"],
            enable_background_tasks=True,
        )
        result = await agent.invoke(
            "Launch 4 background tasks:\n"
            "1. 'sleep 3 && echo WINNER' (background: true)\n"
            "2. 'sleep 60 && echo SLOW_1' (background: true)\n"
            "3. 'sleep 60 && echo SLOW_2' (background: true)\n"
            "4. 'sleep 60 && echo SLOW_3' (background: true)\n"
            "Do 'echo RACE_STARTED'. "
            "Check progress using check_background_progress. "
            "Wait for any task with wait_for_background_task timeout=10 — the first (WINNER) should finish. "
            "Now cancel ALL remaining tasks with cancel_background_task "
            "(cancel each one individually with reason 'Race won, no longer needed'). "
            "Report the winner and which tasks were cancelled."
        )
        log_result(result, "cancel_all_remaining")

        # 4 background launches
        bg_bash = [tc for tc in result.tool_calls
                   if tc.name == "daytona_shell" and tc.input.get("background") is True]
        assert len(bg_bash) >= 4, \
            f"Expected 4+ background launches. Got {len(bg_bash)}: {result.tool_names}"
        assert len(result.background_started()) >= 4, \
            f"Expected 4+ BackgroundTaskStarted events. Got {len(result.background_started())}"
        # wait_for_background_task must be called
        assert result.has_tool("wait_for_background_task"), \
            f"Expected wait_for_background_task. Got: {result.tool_names}"
        # 3+ cancels (the 3 slow tasks)
        cancel_count = result.tool_count("cancel_background_task")
        assert cancel_count >= 3, \
            f"Expected 3+ cancel calls (for 3 slow tasks). Got {cancel_count}: {result.tool_names}"
        # text mentions WINNER
        text_lower = result.text.lower()
        assert any(w in text_lower for w in ["winner", "first"]), \
            f"Expected text to mention WINNER or first. Got: {result.text[:300]}"
        # text mentions cancel
        assert "cancel" in text_lower, \
            f"Expected text to mention cancel. Got: {result.text[:300]}"
        assert not result.has_non_cancel_errors, \
            f"Unexpected errors: {[e.output[:200] for e in result.non_cancel_error_events]}"
