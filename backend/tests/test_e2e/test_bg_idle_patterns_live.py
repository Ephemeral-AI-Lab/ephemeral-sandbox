# ruff: noqa
"""Live E2E: Complex idle and wait patterns with background tasks.

Tests that the LLM correctly handles various idle situations — entering wait mode,
using periodic check-ins, making timeout-based decisions, and managing transitions
between active work and idle monitoring phases.

Run with: .venv/bin/python -m pytest backend/tests/test_e2e/test_bg_idle_patterns_live.py -v -s --log-cli-level=INFO
"""
from __future__ import annotations
import pytest
from engine.testing.eval_agent import EvalAgent
from message.stream_events import ToolExecutionCompleted
from tests.test_e2e.conftest import create_eval_agent, create_test_sandbox, delete_test_sandbox
from tests.test_e2e.helpers import log_result
from tests.test_e2e.bg_prompts import BG_IDLE_PATTERNS

pytestmark = [pytest.mark.e2e, pytest.mark.live]

AGENT_PROMPT = BG_IDLE_PATTERNS


# ===========================================================================
# Test 1: Transition from foreground work to background wait
# ===========================================================================


@pytest.mark.skipif(not EvalAgent.has_all(), reason="API + Daytona both required")
class TestIdleTransitionFromFgToBgWait:
    """LLM completes foreground work then transitions into idle wait mode."""

    @pytest.fixture(scope="class")
    def sandbox(self):
        sb = create_test_sandbox("idle-transition")
        yield sb
        delete_test_sandbox(sb["id"])

    @pytest.mark.asyncio
    async def test_fg_to_idle_wait_transition(self, sandbox):
        """Do foreground tasks, then enter idle wait when all fg work is exhausted."""
        agent = create_eval_agent(
            system_prompt=AGENT_PROMPT,
            sandbox_id=sandbox["id"],
            enable_background_tasks=True,
        )
        result = await agent.invoke(
            "Do these steps in order:\n"
            "1. Launch 'sleep 8 && echo BG_READY' in background (background: true)\n"
            "2. Do foreground work: 'echo TASK_1', 'echo TASK_2', 'echo TASK_3'\n"
            "3. All foreground done. Now enter idle monitoring:\n"
            "4. Call check_background_progress — task should still be running\n"
            "5. Call wait_for_background_task with timeout=12 to block until it finishes\n"
            "6. Report: foreground tasks completed, then waited for bg, final result"
        )
        log_result(result, "fg_to_idle_wait")

        # 1 background launch
        bg_bash = [tc for tc in result.tool_calls
                   if tc.name == "daytona_shell" and tc.input.get("background") is True]
        assert len(bg_bash) >= 1, \
            f"Expected 1+ background launch. Got {len(bg_bash)}"
        assert len(result.background_started()) >= 1, \
            f"Expected BackgroundTaskStarted event. Got: {result.tool_names}"

        # fg bash >= 3
        fg_bash = [tc for tc in result.tool_calls
                   if tc.name == "daytona_shell" and not tc.input.get("background")]
        assert len(fg_bash) >= 3, \
            f"Expected 3+ foreground bash calls (TASK_1/2/3). Got {len(fg_bash)}"

        # has check_background_progress and wait_for_background_task
        assert result.has_tool("check_background_progress"), \
            f"Expected check_background_progress. Got: {result.tool_names}"
        assert result.has_tool("wait_for_background_task"), \
            f"Expected wait_for_background_task. Got: {result.tool_names}"

        # check index < wait index
        check_indices = [i for i, tc in enumerate(result.tool_calls)
                         if tc.name == "check_background_progress"]
        wait_indices = [i for i, tc in enumerate(result.tool_calls)
                        if tc.name == "wait_for_background_task"]
        assert check_indices[0] < wait_indices[0], \
            f"check_background_progress must precede wait_for_background_task. " \
            f"checks={check_indices}, waits={wait_indices}"

        # text mentions "BG_READY" or "complet"
        text_lower = result.text.lower()
        assert any(w in text_lower for w in ["bg_ready", "complet", "finish", "done"]), \
            f"Expected LLM to mention BG_READY or completion. Got: {result.text[:300]}"

        # no non_cancel_errors
        assert not result.has_non_cancel_errors, \
            f"Unexpected errors: {[e.output[:200] for e in result.non_cancel_error_events]}"


# ===========================================================================
# Test 2: Periodic polling with short timeouts
# ===========================================================================


@pytest.mark.skipif(not EvalAgent.has_all(), reason="API + Daytona both required")
class TestIdlePeriodicPolling:
    """LLM uses repeated short-timeout waits to periodically check progress."""

    @pytest.fixture(scope="class")
    def sandbox(self):
        sb = create_test_sandbox("idle-periodic")
        yield sb
        delete_test_sandbox(sb["id"])

    @pytest.mark.asyncio
    async def test_periodic_wait_timeout_check_cycle(self, sandbox):
        """Use multiple short-timeout waits to poll a long-running background task."""
        agent = create_eval_agent(
            system_prompt=AGENT_PROMPT,
            sandbox_id=sandbox["id"],
            enable_background_tasks=True,
        )
        result = await agent.invoke(
            "Launch 'sleep 30 && echo PERIODIC_DONE' in background. Do 'echo START'.\n"
            "You MUST call wait_for_background_task EXACTLY 3 times and check_background_progress "
            "EXACTLY 3 times. Do NOT skip any step even if a previous wait completed unexpectedly:\n"
            "1. check_background_progress\n"
            "2. wait_for_background_task timeout=3 — will timeout (task takes 30s)\n"
            "3. check_background_progress — note elapsed time increased\n"
            "4. wait_for_background_task timeout=3 — will timeout again\n"
            "5. check_background_progress\n"
            "6. wait_for_background_task timeout=30 — should complete this time\n"
            "Report: each wait attempt and when it finally completed."
        )
        log_result(result,"periodic_polling")

        # tool_count("wait_for_background_task") >= 2
        wait_count = result.tool_count("wait_for_background_task")
        assert wait_count >= 2, \
            f"Expected 2+ wait_for_background_task calls (multiple attempts). Got {wait_count}"

        # tool_count("check_background_progress") >= 2
        check_count = result.tool_count("check_background_progress")
        assert check_count >= 2, \
            f"Expected 2+ check_background_progress calls. Got {check_count}"

        # text contains "PERIODIC_DONE" or "complet"
        text_lower = result.text.lower()
        assert any(w in text_lower for w in ["periodic_done", "complet", "finish", "done"]), \
            f"Expected LLM to mention PERIODIC_DONE or completion. Got: {result.text[:300]}"

        # no non_cancel_errors
        assert not result.has_non_cancel_errors, \
            f"Unexpected errors: {[e.output[:200] for e in result.non_cancel_error_events]}"


# ===========================================================================
# Test 3: Pure wait — no foreground work at all
# ===========================================================================


@pytest.mark.skipif(not EvalAgent.has_all(), reason="API + Daytona both required")
class TestIdleNoFgWorkPureWait:
    """No foreground work — LLM launches two bg tasks and manages them purely in idle mode."""

    @pytest.fixture(scope="class")
    def sandbox(self):
        sb = create_test_sandbox("idle-pure-wait")
        yield sb
        delete_test_sandbox(sb["id"])

    @pytest.mark.asyncio
    async def test_pure_idle_no_fg_just_wait(self, sandbox):
        """Launch two bg tasks, wait for first to complete, cancel second. No fg work."""
        agent = create_eval_agent(
            system_prompt=AGENT_PROMPT,
            sandbox_id=sandbox["id"],
            enable_background_tasks=True,
        )
        result = await agent.invoke(
            "There is NO foreground work. Only background monitoring. You MUST complete ALL 7 steps "
            "below in order and MUST NOT skip any step:\n"
            "1. Launch 'sleep 8 && echo PURE_WAIT_DONE' in background (background: true). The tool "
            "result will include a task_id — you MUST copy that exact task_id string and use it as FAST.\n"
            "2. Launch 'sleep 45 && echo SLOW_TASK' in background (background: true). Copy its exact "
            "task_id string and use it as SLOW.\n"
            "3. Call check_background_progress\n"
            "4. Call wait_for_background_task with task_id=<the FAST task_id string from step 1> and "
            "timeout=15. You MUST pass the exact FAST task_id — do NOT pass null/None.\n"
            "5. Call check_background_progress — FAST done, SLOW still running\n"
            "6. Call cancel_background_task with task_id=<the SLOW task_id string from step 2> and "
            "reason='No longer needed'\n"
            "7. Report both task outcomes"
        )
        log_result(result,"pure_idle_wait")

        # 2 background launches
        bg_bash = [tc for tc in result.tool_calls
                   if tc.name == "daytona_shell" and tc.input.get("background") is True]
        assert len(bg_bash) >= 2, \
            f"Expected 2+ background launches. Got {len(bg_bash)}"
        assert len(result.background_started()) >= 2, \
            f"Expected 2 BackgroundTaskStarted events. Got {len(result.background_started())}"

        # fg bash == 0 (no foreground bash work)
        fg_bash = [tc for tc in result.tool_calls
                   if tc.name == "daytona_shell" and not tc.input.get("background")]
        assert len(fg_bash) == 0, \
            f"Expected NO foreground bash calls (pure idle mode). Got {len(fg_bash)}: {[tc.input for tc in fg_bash]}"

        # has check_background_progress, wait_for_background_task, cancel_background_task
        assert result.has_tool("check_background_progress"), \
            f"Expected check_background_progress. Got: {result.tool_names}"
        assert result.has_tool("wait_for_background_task"), \
            f"Expected wait_for_background_task. Got: {result.tool_names}"
        assert result.has_tool("cancel_background_task"), \
            f"Expected cancel_background_task. Got: {result.tool_names}"

        # text mentions completion and cancellation
        text_lower = result.text.lower()
        assert any(w in text_lower for w in ["pure_wait_done", "complet", "finish", "done"]), \
            f"Expected mention of first task completion. Got: {result.text[:300]}"
        assert any(w in text_lower for w in ["cancel", "no longer needed", "slow"]), \
            f"Expected mention of cancellation. Got: {result.text[:300]}"

        # no non_cancel_errors
        assert not result.has_non_cancel_errors, \
            f"Unexpected errors: {[e.output[:200] for e in result.non_cancel_error_events]}"


# ===========================================================================
# Test 4: Wait then resume foreground work
# ===========================================================================


@pytest.mark.skipif(not EvalAgent.has_all(), reason="API + Daytona both required")
class TestIdleWaitThenResumeFg:
    """LLM waits for background task, then uses its result to drive foreground work."""

    @pytest.fixture(scope="class")
    def sandbox(self):
        sb = create_test_sandbox("idle-resume")
        yield sb
        delete_test_sandbox(sb["id"])

    @pytest.mark.asyncio
    async def test_wait_completes_then_resume_fg_work(self, sandbox):
        """Phase: bg + fg prep -> idle wait -> resume fg based on bg result."""
        agent = create_eval_agent(
            system_prompt=AGENT_PROMPT,
            sandbox_id=sandbox["id"],
            enable_background_tasks=True,
        )
        result = await agent.invoke(
            "Phase 1 — Background + foreground:\n"
            "1. Launch 'sleep 4 && echo CONFIG_GENERATED' in background\n"
            "2. Do 'echo PREPARING_ENV'\n"
            "Phase 2 — Idle wait:\n"
            "3. Check progress, then wait with timeout=10\n"
            "Phase 3 — Resume foreground based on bg result:\n"
            "4. The bg task output says CONFIG_GENERATED. Now create /home/daytona/config.json "
            "with content '{\"ready\": true}' using daytona_write_file\n"
            "5. Run 'cat /home/daytona/config.json' to verify\n"
            "Report the three phases."
        )
        log_result(result,"wait_then_resume_fg")

        # 1 background launch
        bg_bash = [tc for tc in result.tool_calls
                   if tc.name == "daytona_shell" and tc.input.get("background") is True]
        assert len(bg_bash) >= 1, \
            f"Expected 1+ background launch. Got {len(bg_bash)}"
        assert len(result.background_started()) >= 1, \
            f"Expected BackgroundTaskStarted event. Got: {result.tool_names}"

        # has wait_for_background_task
        assert result.has_tool("wait_for_background_task"), \
            f"Expected wait_for_background_task. Got: {result.tool_names}"

        # has daytona_write_file with "config.json" in file_path
        write_calls = [tc for tc in result.tool_calls if tc.name == "daytona_write_file"]
        assert len(write_calls) >= 1, \
            f"Expected daytona_write_file. Got: {result.tool_names}"
        assert any("config.json" in tc.input.get("file_path", "") for tc in write_calls), \
            f"Expected config.json write. Got paths: {[tc.input.get('file_path') for tc in write_calls]}"

        # wait index < write index (waited before resuming fg)
        wait_indices = [i for i, tc in enumerate(result.tool_calls)
                        if tc.name == "wait_for_background_task"]
        write_indices = [i for i, tc in enumerate(result.tool_calls)
                         if tc.name == "daytona_write_file"]
        assert wait_indices[0] < write_indices[0], \
            f"wait_for_background_task must precede daytona_write_file. " \
            f"waits={wait_indices}, writes={write_indices}"

        # fg bash includes "cat" and "config.json"
        fg_bash = [tc for tc in result.tool_calls
                   if tc.name == "daytona_shell" and not tc.input.get("background")]
        assert any("cat" in str(tc.input) and "config.json" in str(tc.input) for tc in fg_bash), \
            f"Expected 'cat config.json' verification. Got fg calls: {[tc.input for tc in fg_bash]}"

        # write tool itself must not error; cat failures are tolerated (sandbox write latency)
        write_errors = [
            e for e in result.non_cancel_error_events
            if isinstance(e, ToolExecutionCompleted) and e.tool_name == "daytona_write_file"
        ]
        assert not write_errors, \
            f"daytona_write_file failed: {[e.output[:200] for e in write_errors]}"


# ===========================================================================
# Test 5: Escalating timeout strategy
# ===========================================================================


@pytest.mark.skipif(not EvalAgent.has_all(), reason="API + Daytona both required")
class TestIdleEscalatingTimeout:
    """LLM uses escalating wait timeouts to efficiently monitor a long background task."""

    @pytest.fixture(scope="class")
    def sandbox(self):
        sb = create_test_sandbox("idle-escalate")
        yield sb
        delete_test_sandbox(sb["id"])

    @pytest.mark.asyncio
    async def test_escalating_timeout_strategy(self, sandbox):
        """Use increasing timeout values across multiple wait attempts."""
        agent = create_eval_agent(
            system_prompt=AGENT_PROMPT,
            sandbox_id=sandbox["id"],
            enable_background_tasks=True,
        )
        result = await agent.invoke(
            "Launch 'sleep 30 && echo ESCALATED_DONE' in background. Do 'echo MONITOR'.\n"
            "You MUST call wait_for_background_task EXACTLY 3 times with escalating timeouts. "
            "Do NOT skip any step even if a previous wait completed unexpectedly:\n"
            "1. check_background_progress\n"
            "2. wait_for_background_task timeout=2 — this WILL timeout (task takes 30s)\n"
            "3. check_background_progress — note elapsed time\n"
            "4. wait_for_background_task timeout=3 — this WILL timeout again\n"
            "5. check_background_progress — note elapsed time increasing\n"
            "6. wait_for_background_task timeout=30 — should finally complete\n"
            "Report: each timeout attempt and when it finally succeeded."
        )
        log_result(result,"escalating_timeout")

        # tool_count("wait_for_background_task") >= 3
        wait_count = result.tool_count("wait_for_background_task")
        assert wait_count >= 3, \
            f"Expected 3+ wait_for_background_task calls (escalating). Got {wait_count}"

        # tool_count("check_background_progress") >= 2
        check_count = result.tool_count("check_background_progress")
        assert check_count >= 2, \
            f"Expected 2+ check_background_progress calls. Got {check_count}"

        # text contains "ESCALATED_DONE" or "complet"
        text_lower = result.text.lower()
        assert any(w in text_lower for w in ["escalated_done", "complet", "finish", "done", "success"]), \
            f"Expected LLM to mention ESCALATED_DONE or completion. Got: {result.text[:300]}"

        # no non_cancel_errors
        assert not result.has_non_cancel_errors, \
            f"Unexpected errors: {[e.output[:200] for e in result.non_cancel_error_events]}"


# ===========================================================================
# Test 6: Multiple staggered background tasks with pure idle monitoring
# ===========================================================================


@pytest.mark.skipif(not EvalAgent.has_all(), reason="API + Daytona both required")
class TestIdleMultipleBgStaggeredWait:
    """Three staggered background tasks — wait for each in order, cancel the last."""

    @pytest.fixture(scope="class")
    def sandbox(self):
        sb = create_test_sandbox("idle-multi-stagger")
        yield sb
        delete_test_sandbox(sb["id"])

    @pytest.mark.asyncio
    async def test_idle_wait_staggered_multiple_bg(self, sandbox):
        """Launch 3 staggered bg tasks, wait for first two, cancel the third."""
        agent = create_eval_agent(
            system_prompt=AGENT_PROMPT,
            sandbox_id=sandbox["id"],
            enable_background_tasks=True,
        )
        result = await agent.invoke(
            "Launch 3 background tasks with staggered durations. For each launch, the tool result "
            "includes a task_id — you MUST copy that exact task_id string to use in later steps:\n"
            "1. 'sleep 5 && echo ALPHA_DONE' (background: true) — save its task_id as ALPHA\n"
            "2. 'sleep 15 && echo BETA_DONE' (background: true) — save its task_id as BETA\n"
            "3. 'sleep 60 && echo GAMMA_DONE' (background: true) — save its task_id as GAMMA\n"
            "No foreground work — pure idle monitoring. You MUST complete ALL steps below in order "
            "and MUST NOT skip any step, especially the final cancel:\n"
            "4. check_background_progress — all 3 running\n"
            "5. wait_for_background_task with task_id=<ALPHA string>, timeout=12 — ALPHA should finish\n"
            "6. check_background_progress — ALPHA done, BETA/GAMMA running\n"
            "7. wait_for_background_task with task_id=<BETA string>, timeout=20 — BETA should finish\n"
            "8. check_background_progress — ALPHA/BETA done, GAMMA still running\n"
            "9. cancel_background_task with task_id=<GAMMA string>, reason='Taking too long'. "
            "This cancel step is MANDATORY — you MUST call cancel_background_task before reporting.\n"
            "10. Report: completion order and final states"
        )
        log_result(result,"staggered_multi_bg")

        # 3 background launches
        bg_bash = [tc for tc in result.tool_calls
                   if tc.name == "daytona_shell" and tc.input.get("background") is True]
        assert len(bg_bash) >= 3, \
            f"Expected 3+ background launches. Got {len(bg_bash)}"
        assert len(result.background_started()) >= 3, \
            f"Expected 3 BackgroundTaskStarted events. Got {len(result.background_started())}"

        # fg bash == 0 (pure idle)
        fg_bash = [tc for tc in result.tool_calls
                   if tc.name == "daytona_shell" and not tc.input.get("background")]
        assert len(fg_bash) == 0, \
            f"Expected NO foreground bash calls (pure idle mode). Got {len(fg_bash)}: {[tc.input for tc in fg_bash]}"

        # tool_count("wait_for_background_task") >= 2
        wait_count = result.tool_count("wait_for_background_task")
        assert wait_count >= 2, \
            f"Expected 2+ wait_for_background_task calls. Got {wait_count}"

        # tool_count("check_background_progress") >= 3
        check_count = result.tool_count("check_background_progress")
        assert check_count >= 3, \
            f"Expected 3+ check_background_progress calls. Got {check_count}"

        # has cancel_background_task — cancelled GAMMA
        assert result.has_tool("cancel_background_task"), \
            f"Expected cancel_background_task (GAMMA). Got: {result.tool_names}"

        cancel_calls = [tc for tc in result.tool_calls if tc.name == "cancel_background_task"]
        assert cancel_calls[0].input.get("reason"), \
            f"Expected cancel with reason. Got: {cancel_calls[0].input}"

        # text mentions completion of first two and cancellation of third
        text_lower = result.text.lower()
        assert any(w in text_lower for w in ["alpha", "alpha_done", "first"]), \
            f"Expected mention of ALPHA completion. Got: {result.text[:300]}"
        assert any(w in text_lower for w in ["beta", "beta_done", "second"]), \
            f"Expected mention of BETA completion. Got: {result.text[:300]}"
        assert any(w in text_lower for w in ["cancel", "gamma", "too long"]), \
            f"Expected mention of GAMMA cancellation. Got: {result.text[:300]}"

        # no non_cancel_errors
        assert not result.has_non_cancel_errors, \
            f"Unexpected errors: {[e.output[:200] for e in result.non_cancel_error_events]}"
