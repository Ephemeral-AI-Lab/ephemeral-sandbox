# ruff: noqa
"""Live E2E: Agent autonomous decision-making with background tasks.

Tests that the agent makes intelligent decisions based on background task
status: cancelling slow tasks, acting on results, selective cancellation,
periodic check-ins, chained workflows, and full pipelines.

Run with: .venv/bin/python -m pytest backend/tests/test_e2e/test_bg_autonomous_decisions_live.py -v -s --log-cli-level=INFO
"""

from __future__ import annotations

import pytest

from engine.testing.eval_agent import EvalAgent
from tests.test_e2e.conftest import create_eval_agent, create_test_sandbox, delete_test_sandbox
from tests.test_e2e.helpers import log_result
from tests.test_e2e.bg_prompts import _build_prompt

pytestmark = [pytest.mark.e2e, pytest.mark.live]

AGENT_PROMPT = _build_prompt(
    agent_name="test-decision-agent",
    extra_sections=(
        "- Use check_background_progress for instant status snapshots.\n"
        "- Use wait_for_background_task to block when you have no foreground work.\n"
        "- Use cancel_background_task to cancel tasks.\n"
        "- Make autonomous decisions based on task status and output.\n"
        "\n"
        "BACKGROUND WORKFLOW:\n"
        "1. Launch background tasks, do foreground work\n"
        "2. Check progress, then wait when idle\n"
        "3. Make decisions based on results: cancel slow tasks, act on output, create files"
    ),
)


# ===========================================================================
# Test 1: Cancel after wait timeout — slow task gets cancelled
# ===========================================================================


@pytest.mark.skipif(not EvalAgent.has_all(), reason="API + Daytona both required")
class TestDecisionCancelSlowAfterWaitTimeout:
    """Agent cancels a slow background task after wait_for_background_task times out."""

    @pytest.fixture(scope="class")
    def sandbox(self):
        sb = create_test_sandbox("decision-cancel-slow")
        yield sb
        delete_test_sandbox(sb["id"])

    @pytest.mark.asyncio
    async def test_agent_cancels_after_wait_timeout(self, sandbox):
        """Launch slow bg task, wait with short timeout, cancel when it times out."""
        agent = create_eval_agent(
            system_prompt=AGENT_PROMPT,
            sandbox_id=sandbox["id"],
            enable_background_tasks=True,
        )
        result = await agent.invoke(
            "Launch 'sleep 60 && echo SLOW_BUILD' in background (background: true). "
            "Do 'echo FG' in foreground. "
            "Check progress with check_background_progress. "
            "Call wait_for_background_task with timeout=5. "
            "When the wait times out and the task is still running, you should decide to cancel it "
            "because it's too slow. Cancel with reason \"Build too slow\". "
            "Report your decision."
        )
        log_result(result, "cancel_slow_after_timeout")

        assert result.has_tool("check_background_progress"), \
            f"Expected check_background_progress. Got: {result.tool_names}"
        assert result.has_tool("wait_for_background_task"), \
            f"Expected wait_for_background_task. Got: {result.tool_names}"
        assert result.has_tool("cancel_background_task"), \
            f"Expected cancel_background_task. Got: {result.tool_names}"
        # Cancel call must include a reason field
        cancel_calls = [tc for tc in result.tool_calls if tc.name == "cancel_background_task"]
        assert any(tc.input.get("reason") for tc in cancel_calls), \
            f"Expected cancel with reason. Got: {[tc.input for tc in cancel_calls]}"
        # wait must appear before cancel in tool sequence
        wait_indices = [i for i, tc in enumerate(result.tool_calls)
                        if tc.name == "wait_for_background_task"]
        cancel_indices = [i for i, tc in enumerate(result.tool_calls)
                          if tc.name == "cancel_background_task"]
        assert wait_indices and cancel_indices, \
            f"Need both wait and cancel calls. tool_names={result.tool_names}"
        assert wait_indices[0] < cancel_indices[0], \
            f"wait_for_background_task must precede cancel_background_task. " \
            f"wait={wait_indices}, cancel={cancel_indices}"
        # LLM report must acknowledge the decision
        text_lower = result.text.lower()
        assert any(w in text_lower for w in ["cancel", "slow", "timeout"]), \
            f"Expected text mentioning cancel/slow/timeout. Got: {result.text[:300]}"
        assert not result.has_non_cancel_errors, \
            f"Unexpected errors: {[e.output[:200] for e in result.non_cancel_error_events]}"


# ===========================================================================
# Test 2: Act on completed wait result — write file when build succeeds
# ===========================================================================


@pytest.mark.skipif(not EvalAgent.has_all(), reason="API + Daytona both required")
class TestDecisionActOnWaitResult:
    """Agent acts on background task result after wait_for_background_task completes."""

    @pytest.fixture(scope="class")
    def sandbox(self):
        sb = create_test_sandbox("decision-act-result")
        yield sb
        delete_test_sandbox(sb["id"])

    @pytest.mark.asyncio
    async def test_agent_acts_on_completed_result(self, sandbox):
        """Wait for bg build to succeed, then create a deploy file based on the result."""
        agent = create_eval_agent(
            system_prompt=AGENT_PROMPT,
            sandbox_id=sandbox["id"],
            enable_background_tasks=True,
        )
        result = await agent.invoke(
            "Launch 'sleep 3 && echo BUILD_SUCCESS' in background (background: true). "
            "Do 'echo PREPARING' in foreground. "
            "Check progress with check_background_progress. "
            "Call wait_for_background_task with timeout=10. "
            "When the task completes with BUILD_SUCCESS, create /home/daytona/deploy.txt "
            "with content \"DEPLOY_READY\" using daytona_write_file. "
            "Then verify with 'cat /home/daytona/deploy.txt'. "
            "Report the full workflow."
        )
        log_result(result, "act_on_result")

        assert result.has_tool("wait_for_background_task"), \
            f"Expected wait_for_background_task. Got: {result.tool_names}"
        assert result.has_tool("daytona_write_file"), \
            f"Expected daytona_write_file. Got: {result.tool_names}"
        # Write call must target deploy.txt with DEPLOY_READY content
        write_calls = [tc for tc in result.tool_calls if tc.name == "daytona_write_file"]
        assert any("deploy" in tc.input.get("file_path", "") for tc in write_calls), \
            f"Expected write to deploy file. Got paths: {[tc.input.get('file_path') for tc in write_calls]}"
        assert any("DEPLOY_READY" in tc.input.get("content", "") for tc in write_calls), \
            f"Expected DEPLOY_READY content. Got: {[tc.input.get('content') for tc in write_calls]}"
        # Verification cat of deploy.txt must appear in fg bash calls
        fg_bash = [tc for tc in result.tool_calls
                   if tc.name == "daytona_shell" and not tc.input.get("background")]
        assert any("cat" in str(tc.input) and "deploy" in str(tc.input) for tc in fg_bash), \
            f"Expected 'cat deploy.txt' verification. Got fg calls: {[tc.input for tc in fg_bash]}"
        # wait must appear before write in tool sequence
        wait_indices = [i for i, tc in enumerate(result.tool_calls)
                        if tc.name == "wait_for_background_task"]
        write_indices = [i for i, tc in enumerate(result.tool_calls)
                         if tc.name == "daytona_write_file"]
        assert wait_indices and write_indices, \
            f"Need both wait and write calls. tool_names={result.tool_names}"
        assert wait_indices[0] < write_indices[0], \
            f"wait_for_background_task must precede daytona_write_file. " \
            f"wait={wait_indices}, write={write_indices}"
        assert not result.unrecovered_error_events, \
            f"Unexpected errors: {[e.output[:200] for e in result.unrecovered_error_events]}"


# ===========================================================================
# Test 3: Selective cancel — cancel only the slowest task
# ===========================================================================


@pytest.mark.skipif(not EvalAgent.has_all(), reason="API + Daytona both required")
class TestDecisionSelectiveCancel:
    """Agent cancels only the slowest task while letting faster ones run."""

    @pytest.fixture(scope="class")
    def sandbox(self):
        sb = create_test_sandbox("decision-selective")
        yield sb
        delete_test_sandbox(sb["id"])

    @pytest.mark.asyncio
    async def test_agent_cancels_slowest_keeps_fast(self, sandbox):
        """Launch 3 bg tasks with different durations, cancel only the 60s one."""
        agent = create_eval_agent(
            system_prompt=AGENT_PROMPT,
            sandbox_id=sandbox["id"],
            enable_background_tasks=True,
        )
        result = await agent.invoke(
            "Launch 3 background tasks:\n"
            "1. 'sleep 3 && echo FAST_DONE' in background (background: true)\n"
            "2. 'sleep 6 && echo MED_DONE' in background (background: true)\n"
            "3. 'sleep 60 && echo SLOW_DONE' in background (background: true)\n"
            "Do 'echo START' in foreground. "
            "Check progress with check_background_progress. "
            "Call wait_for_background_task with timeout=10 to wait for the fast task. "
            "After it finishes, check progress again. "
            "The slow task (60s) is still running — cancel ONLY the slow one, let the medium one finish. "
            "Wait for the medium task with wait_for_background_task. "
            "Report: which completed, which was cancelled."
        )
        log_result(result, "selective_cancel")

        # Must have launched 3 background tasks
        bg_bash = [tc for tc in result.tool_calls
                   if tc.name == "daytona_shell" and tc.input.get("background") is True]
        assert len(bg_bash) >= 3, \
            f"Expected 3+ background launches. Got {len(bg_bash)}"
        assert result.has_tool("wait_for_background_task"), \
            f"Expected wait_for_background_task. Got: {result.tool_names}"
        waits = result.tool_count("wait_for_background_task")
        assert waits >= 1, \
            f"Expected 1+ wait_for_background_task calls. Got {waits}"
        assert result.has_tool("cancel_background_task"), \
            f"Expected cancel_background_task. Got: {result.tool_names}"
        # Only the slow task should be cancelled — exactly 1 cancel
        cancel_calls = [tc for tc in result.tool_calls if tc.name == "cancel_background_task"]
        assert len(cancel_calls) == 1, \
            f"Expected exactly 1 cancel (the slow 60s task). Got {len(cancel_calls)}: {[tc.input for tc in cancel_calls]}"
        # LLM text must acknowledge completing tasks and the cancellation
        text_lower = result.text.lower()
        assert any(w in text_lower for w in ["complet", "done", "finish", "fast", "med"]), \
            f"Expected text mentioning completed tasks. Got: {result.text[:300]}"
        assert any(w in text_lower for w in ["cancel", "slow"]), \
            f"Expected text mentioning cancel/slow. Got: {result.text[:300]}"
        assert not result.has_non_cancel_errors, \
            f"Unexpected errors: {[e.output[:200] for e in result.non_cancel_error_events]}"


# ===========================================================================
# Test 4: Periodic check-in — two wait attempts with timeout
# ===========================================================================


@pytest.mark.skipif(not EvalAgent.has_all(), reason="API + Daytona both required")
class TestDecisionPeriodicCheckIn:
    """Agent uses wait_for_background_task as periodic check-ins on a long task."""

    @pytest.fixture(scope="class")
    def sandbox(self):
        sb = create_test_sandbox("decision-periodic")
        yield sb
        delete_test_sandbox(sb["id"])

    @pytest.mark.asyncio
    async def test_agent_uses_wait_as_periodic_checkin(self, sandbox):
        """First wait times out (3s timeout on 8s task), second wait completes."""
        agent = create_eval_agent(
            system_prompt=AGENT_PROMPT,
            sandbox_id=sandbox["id"],
            enable_background_tasks=True,
        )
        result = await agent.invoke(
            "Launch 'sleep 8 && echo DEPLOY_DONE' in background (background: true). "
            "Do 'echo MONITORING' in foreground. "
            "Check progress with check_background_progress. "
            "You MUST call wait_for_background_task EXACTLY with timeout=3 first. "
            "This WILL timeout because the task takes 8 seconds — that is expected. "
            "After the timeout, call check_background_progress again to see the task is still running. "
            "Then call wait_for_background_task a SECOND time with timeout=15. "
            "This second wait MUST be a separate call — do NOT skip it or combine with the first. "
            "The second wait should complete successfully with DEPLOY_DONE. "
            "Report: how many wait attempts you made, and the final result."
        )
        log_result(result, "periodic_checkin")

        # Must have called wait at least twice (first times out, second completes)
        waits = result.tool_count("wait_for_background_task")
        assert waits >= 2, \
            f"Expected 2+ wait_for_background_task calls (one timeout + one success). Got {waits}"
        # Must have checked progress at least twice
        checks = result.tool_count("check_background_progress")
        assert checks >= 2, \
            f"Expected 2+ check_background_progress calls. Got {checks}"
        # LLM text must acknowledge the final completion
        text_lower = result.text.lower()
        assert any(w in text_lower for w in ["deploy_done", "complet", "done", "finish"]), \
            f"Expected text mentioning DEPLOY_DONE or completion. Got: {result.text[:300]}"
        assert not result.has_non_cancel_errors, \
            f"Unexpected errors: {[e.output[:200] for e in result.non_cancel_error_events]}"


# ===========================================================================
# Test 5: Chained workflow — bg result triggers file write then second bg task
# ===========================================================================


@pytest.mark.skipif(not EvalAgent.has_all(), reason="API + Daytona both required")
class TestDecisionChainedWorkflow:
    """Agent chains background results into sequential actions across multiple phases."""

    @pytest.fixture(scope="class")
    def sandbox(self):
        sb = create_test_sandbox("decision-chained")
        yield sb
        delete_test_sandbox(sb["id"])

    @pytest.mark.asyncio
    async def test_agent_chains_bg_result_to_next_action(self, sandbox):
        """Tests pass -> write report -> launch deploy bg -> wait -> verify."""
        agent = create_eval_agent(
            system_prompt=AGENT_PROMPT,
            sandbox_id=sandbox["id"],
            enable_background_tasks=True,
        )
        result = await agent.invoke(
            "Execute this pipeline:\n"
            "Step 1: Launch 'sleep 3 && echo \"TESTS_PASSED exit_code=0\"' in background (background: true). "
            "Do 'echo BUILDING' in foreground. "
            "Check progress with check_background_progress. "
            "Call wait_for_background_task with timeout=10.\n"
            "Step 2: The output says TESTS_PASSED — create /home/daytona/test_report.txt "
            "with content \"All tests passed. Ready for deploy.\" using daytona_write_file.\n"
            "Step 3: Launch 'sleep 2 && echo DEPLOY_COMPLETE' in background (background: true). "
            "Call wait_for_background_task with timeout=10.\n"
            "Step 4: Verify deploy by running 'cat /home/daytona/test_report.txt' in foreground. "
            "Report complete pipeline status."
        )
        log_result(result, "chained_workflow")

        # Must have launched at least 2 background tasks (tests + deploy)
        assert len(result.background_started()) >= 2, \
            f"Expected 2+ background tasks (tests + deploy). Got {len(result.background_started())}"
        assert result.has_tool("daytona_write_file"), \
            f"Expected daytona_write_file. Got: {result.tool_names}"
        # Write must target test_report.txt
        write_calls = [tc for tc in result.tool_calls if tc.name == "daytona_write_file"]
        assert any("test_report" in tc.input.get("file_path", "") for tc in write_calls), \
            f"Expected write to test_report.txt. Got paths: {[tc.input.get('file_path') for tc in write_calls]}"
        # Must have called wait at least twice (one per phase)
        waits = result.tool_count("wait_for_background_task")
        assert waits >= 2, \
            f"Expected 2+ wait_for_background_task calls. Got {waits}"
        # Verification cat of test_report.txt must appear
        fg_bash = [tc for tc in result.tool_calls
                   if tc.name == "daytona_shell" and not tc.input.get("background")]
        assert any("cat" in str(tc.input) and "test_report" in str(tc.input) for tc in fg_bash), \
            f"Expected 'cat test_report.txt' verification. Got fg calls: {[tc.input for tc in fg_bash]}"
        # LLM text must acknowledge the pipeline
        text_lower = result.text.lower()
        assert any(w in text_lower for w in ["pipeline", "deploy", "passed", "complete"]), \
            f"Expected text mentioning pipeline/deploy/passed/complete. Got: {result.text[:300]}"
        assert not result.has_non_cancel_errors, \
            f"Unexpected errors: {[e.output[:200] for e in result.non_cancel_error_events]}"


# ===========================================================================
# Test 6: Full pipeline — build + slow lint, cancel lint, write result, verify
# ===========================================================================


@pytest.mark.skipif(not EvalAgent.has_all(), reason="API + Daytona both required")
class TestDecisionFullPipeline:
    """Full pipeline: two bg tasks, fg work, check, wait, cancel slow, write, verify."""

    @pytest.fixture(scope="class")
    def sandbox(self):
        sb = create_test_sandbox("decision-pipeline")
        yield sb
        delete_test_sandbox(sb["id"])

    @pytest.mark.asyncio
    async def test_full_bg_tool_pipeline(self, sandbox):
        """Complete pipeline exercising all background task decision tools."""
        agent = create_eval_agent(
            system_prompt=AGENT_PROMPT,
            sandbox_id=sandbox["id"],
            enable_background_tasks=True,
        )
        result = await agent.invoke(
            "Complete this pipeline:\n"
            "1. Launch 'sleep 3 && echo BUILD_OK' in background (background: true)\n"
            "2. Launch 'sleep 45 && echo SLOW_LINT' in background (background: true)\n"
            "3. Do foreground work: 'echo COMPILE_START' and 'echo COMPILE_DONE'\n"
            "4. Call check_background_progress to see both tasks\n"
            "5. Call wait_for_background_task with timeout=10 — build should finish\n"
            "6. Check progress again — build done, lint still running\n"
            "7. Cancel the slow lint with reason \"Lint taking too long\"\n"
            "8. Create /home/daytona/pipeline_result.txt with \"Build: OK, Lint: Cancelled\" "
            "using daytona_write_file\n"
            "9. Verify with 'cat /home/daytona/pipeline_result.txt'\n"
            "10. Report final summary"
        )
        log_result(result, "full_pipeline")

        # Must have launched 2+ background tasks
        bg_bash = [tc for tc in result.tool_calls
                   if tc.name == "daytona_shell" and tc.input.get("background") is True]
        assert len(bg_bash) >= 2, \
            f"Expected 2+ background launches. Got {len(bg_bash)}"
        # Must have done 2+ foreground bash calls
        fg_bash = [tc for tc in result.tool_calls
                   if tc.name == "daytona_shell" and not tc.input.get("background")]
        assert len(fg_bash) >= 2, \
            f"Expected 2+ foreground bash calls. Got {len(fg_bash)}"
        # Must have checked progress at least twice (steps 4 and 6)
        checks = result.tool_count("check_background_progress")
        assert checks >= 2, \
            f"Expected 2+ check_background_progress calls. Got {checks}"
        assert result.has_tool("wait_for_background_task"), \
            f"Expected wait_for_background_task. Got: {result.tool_names}"
        assert result.has_tool("cancel_background_task"), \
            f"Expected cancel_background_task. Got: {result.tool_names}"
        assert result.has_tool("daytona_write_file"), \
            f"Expected daytona_write_file. Got: {result.tool_names}"
        # Write must target pipeline_result.txt
        write_calls = [tc for tc in result.tool_calls if tc.name == "daytona_write_file"]
        assert any("pipeline_result" in tc.input.get("file_path", "") for tc in write_calls), \
            f"Expected write to pipeline_result.txt. Got paths: {[tc.input.get('file_path') for tc in write_calls]}"
        # Ordering: first check must precede first wait
        check_indices = [i for i, tc in enumerate(result.tool_calls)
                         if tc.name == "check_background_progress"]
        wait_indices = [i for i, tc in enumerate(result.tool_calls)
                        if tc.name == "wait_for_background_task"]
        cancel_indices = [i for i, tc in enumerate(result.tool_calls)
                          if tc.name == "cancel_background_task"]
        assert check_indices and wait_indices and cancel_indices, \
            f"Need check, wait, and cancel calls. tool_names={result.tool_names}"
        assert check_indices[0] < wait_indices[0], \
            f"check_background_progress must precede wait_for_background_task. " \
            f"checks={check_indices}, waits={wait_indices}"
        assert wait_indices[0] < cancel_indices[0], \
            f"wait_for_background_task must precede cancel_background_task. " \
            f"waits={wait_indices}, cancels={cancel_indices}"
        # LLM text must acknowledge both build and lint/cancel
        text_lower = result.text.lower()
        assert any(w in text_lower for w in ["build", "build_ok", "ok"]), \
            f"Expected text mentioning build. Got: {result.text[:300]}"
        assert any(w in text_lower for w in ["lint", "cancel"]), \
            f"Expected text mentioning lint/cancel. Got: {result.text[:300]}"
        assert not result.has_non_cancel_errors, \
            f"Unexpected errors: {[e.output[:200] for e in result.non_cancel_error_events]}"
