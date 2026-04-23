# ruff: noqa
"""Live E2E: High-concurrency background + foreground task mixing.

Tests that the LLM correctly manages multiple simultaneous background tasks
alongside multiple foreground operations under high concurrency pressure.

Run with: .venv/bin/python -m pytest backend/tests/test_e2e/test_bg_high_concurrency_live.py -v -s --log-cli-level=INFO
"""

from __future__ import annotations

import pytest

from engine.testing.eval_agent import EvalAgent
from tests.test_e2e.bg_prompts import BG_CONCURRENCY
from tests.test_e2e.conftest import create_eval_agent, create_test_sandbox, delete_test_sandbox
from tests.test_e2e.helpers import assert_fg_during_bg, log_result

pytestmark = [pytest.mark.e2e, pytest.mark.live]

AGENT_PROMPT = BG_CONCURRENCY


# ===========================================================================
# Test 1: Three simultaneous background tasks
# ===========================================================================


@pytest.mark.skipif(not EvalAgent.has_all(), reason="API + Daytona both required")
class TestTripleBackgroundConcurrency:
    """Launch 3 background tasks simultaneously and manage them."""

    @pytest.fixture(scope="class")
    def sandbox(self):
        sb = create_test_sandbox("bg-triple")
        yield sb
        delete_test_sandbox(sb["id"])

    @pytest.mark.asyncio
    async def test_three_background_tasks_launched(self, sandbox):
        """LLM should launch 3 background tasks and do foreground work."""
        agent = create_eval_agent(
            system_prompt=AGENT_PROMPT,
            sandbox_id=sandbox["id"],
            enable_background_tasks=True,
        )
        result = await agent.invoke(
            "Launch THREE background tasks simultaneously:\n"
            "1. Run 'sleep 10 && echo BUILD_A_DONE' in background (background: true)\n"
            "2. Run 'sleep 15 && echo BUILD_B_DONE' in background (background: true)\n"
            "3. Run 'sleep 20 && echo BUILD_C_DONE' in background (background: true)\n\n"
            "While those run, do these foreground tasks:\n"
            "4. Run 'echo FG_WORK_1' in foreground\n"
            "5. Run 'echo FG_WORK_2' in foreground\n\n"
            "Then check progress on all background tasks using check_background_progress.\n"
            "Use background: true for steps 1-3 ONLY."
        )
        log_result(result, "triple_bg")

        assert len(result.assistant_turns()) >= 1, "Missing assistant turn"
        # Strict: exactly 3 background tasks must be launched
        bg_started = result.background_started()
        assert len(bg_started) >= 3, \
            f"Expected 3+ background tasks started. Got {len(bg_started)}"
        # Strict: all 3 bg tasks must use background: true on daytona_shell
        bg_bash_calls = [tc for tc in result.tool_calls
                         if tc.name == "daytona_shell" and tc.input.get("background") is True]
        assert len(bg_bash_calls) >= 3, \
            f"Expected 3+ daytona_shell calls with background: true. Got {len(bg_bash_calls)}: {result.tool_calls}"
        # Strict: foreground calls must NOT have background flag
        fg_bash_calls = [tc for tc in result.tool_calls
                         if tc.name == "daytona_shell" and not tc.input.get("background")]
        assert len(fg_bash_calls) >= 2, \
            f"Expected 2+ foreground bash calls (no background flag). Got {len(fg_bash_calls)}"
        # Background launches appear as BackgroundTaskStarted, not ToolExecutionStarted
        # Foreground total: 2 fg bash + 1 check = 3+
        assert len(result.tools_started()) >= 3, \
            f"Expected 3+ foreground tool calls. Got: {result.tool_names}"
        assert len(result.tools_started()) + len(result.background_started()) >= 6, \
            f"Expected 6+ total actions (fg + bg). Got {len(result.tools_started())} fg + {len(result.background_started())} bg"
        assert result.has_tool("check_background_progress"), \
            f"Expected check_background_progress. Got: {result.tool_names}"
        # Strict: fg calls must happen WHILE bg tasks are running (true concurrency)
        assert_fg_during_bg(result, min_fg=2)
        assert not result.has_non_cancel_errors, \
            f"Unexpected errors: {[e.output[:200] for e in result.non_cancel_error_events]}"


# ===========================================================================
# Test 2: Interleaved background launches with foreground work
# ===========================================================================


@pytest.mark.skipif(not EvalAgent.has_all(), reason="API + Daytona both required")
class TestInterleavedBgFg:
    """Background tasks launched between foreground operations."""

    @pytest.fixture(scope="class")
    def sandbox(self):
        sb = create_test_sandbox("bg-interleave")
        yield sb
        delete_test_sandbox(sb["id"])

    @pytest.mark.asyncio
    async def test_interleaved_bg_fg_execution(self, sandbox):
        """Alternate between launching bg tasks and doing fg work."""
        agent = create_eval_agent(
            system_prompt=AGENT_PROMPT,
            sandbox_id=sandbox["id"],
            enable_background_tasks=True,
        )
        result = await agent.invoke(
            "Execute these steps IN ORDER:\n"
            "1. Run 'echo SETUP_DONE' in foreground\n"
            "2. Run 'sleep 15 && echo TESTS_DONE' in background (background: true)\n"
            "3. Run 'echo LINT_STARTED' in foreground\n"
            "4. Run 'sleep 20 && echo DEPLOY_DONE' in background (background: true)\n"
            "5. Run 'echo CONFIG_UPDATED' in foreground\n"
            "6. Check progress of all background tasks using check_background_progress\n"
            "7. Cancel all background tasks using cancel_background_task\n\n"
            "Use background: true ONLY for steps 2 and 4."
        )
        log_result(result, "interleaved")

        # Strict: exactly 2 background launches
        bg_bash = [tc for tc in result.tool_calls
                   if tc.name == "daytona_shell" and tc.input.get("background") is True]
        assert len(bg_bash) >= 2, \
            f"Expected 2+ daytona_shell with background: true. Got {len(bg_bash)}"
        # Strict: 3+ foreground bash calls without background flag
        fg_bash = [tc for tc in result.tool_calls
                   if tc.name == "daytona_shell" and not tc.input.get("background")]
        assert len(fg_bash) >= 3, \
            f"Expected 3+ foreground bash calls. Got {len(fg_bash)}"
        # Strict: interleaving order — first fg call should appear before second bg call
        all_bash = [(i, tc) for i, tc in enumerate(result.tool_calls) if tc.name == "daytona_shell"]
        bg_indices = [i for i, tc in all_bash if tc.input.get("background") is True]
        fg_indices = [i for i, tc in all_bash if not tc.input.get("background")]
        if len(bg_indices) >= 2 and len(fg_indices) >= 1:
            assert fg_indices[0] < bg_indices[-1], \
                f"Expected interleaved order (fg before last bg). bg={bg_indices}, fg={fg_indices}"
        assert result.has_tool("check_background_progress"), \
            f"Expected progress check. Got: {result.tool_names}"
        assert result.has_tool("cancel_background_task"), \
            f"Expected cancel. Got: {result.tool_names}"
        # Strict: fg calls must happen WHILE bg tasks are running (true concurrency)
        assert_fg_during_bg(result, min_fg=1)
        assert not result.has_non_cancel_errors, \
            f"Unexpected errors: {[e.output[:200] for e in result.non_cancel_error_events]}"


# ===========================================================================
# Test 3: Multiple background tasks with file creation foreground
# ===========================================================================


@pytest.mark.skipif(not EvalAgent.has_all(), reason="API + Daytona both required")
class TestBgWithFileCreation:
    """Background builds while creating files in foreground."""

    @pytest.fixture(scope="class")
    def sandbox(self):
        sb = create_test_sandbox("bg-files")
        yield sb
        delete_test_sandbox(sb["id"])

    @pytest.mark.asyncio
    async def test_bg_tasks_with_fg_file_operations(self, sandbox):
        """Launch bg tasks, create multiple files in fg, then check/cancel."""
        agent = create_eval_agent(
            system_prompt=AGENT_PROMPT,
            sandbox_id=sandbox["id"],
            enable_background_tasks=True,
        )
        result = await agent.invoke(
            "Do the following:\n"
            "1. Run 'sleep 15 && echo COMPILE_DONE' in background (background: true)\n"
            "2. Run 'sleep 25 && echo PACKAGE_DONE' in background (background: true)\n"
            "3. Create /home/daytona/config.json with '{\"version\": 1}' using daytona_write_file\n"
            "4. Create /home/daytona/readme.txt with 'Project README' using daytona_write_file\n"
            "5. Run 'ls /home/daytona/' in foreground to verify files\n"
            "6. Check background progress using check_background_progress\n"
            "7. Cancel all background tasks\n\n"
            "Use background: true for steps 1-2 ONLY."
        )
        log_result(result, "bg_files")

        # Strict: 2 background tasks with background: true
        bg_bash = [tc for tc in result.tool_calls
                   if tc.name == "daytona_shell" and tc.input.get("background") is True]
        assert len(bg_bash) >= 2, \
            f"Expected 2+ daytona_shell with background: true. Got {len(bg_bash)}"
        assert len(result.background_started()) >= 2, \
            f"Expected 2+ BackgroundTaskStarted events. Got {len(result.background_started())}"
        # Strict: exactly 2 file writes with correct paths
        write_calls = [tc for tc in result.tool_calls if tc.name == "daytona_write_file"]
        assert len(write_calls) >= 2, \
            f"Expected 2+ daytona_write_file calls. Got {len(write_calls)}"
        write_paths = [tc.input.get("file_path", "") for tc in write_calls]
        assert any("config.json" in p for p in write_paths), \
            f"Expected config.json write. Got paths: {write_paths}"
        assert any("readme.txt" in p for p in write_paths), \
            f"Expected readme.txt write. Got paths: {write_paths}"
        # Strict: verification ls command ran in foreground
        fg_bash = [tc for tc in result.tool_calls
                   if tc.name == "daytona_shell" and not tc.input.get("background")]
        assert any("ls" in str(tc.input) for tc in fg_bash), \
            f"Expected 'ls' foreground command for verification. Got fg calls: {[tc.input for tc in fg_bash]}"
        assert result.has_tool("check_background_progress"), \
            f"Expected progress check. Got: {result.tool_names}"
        assert result.has_tool("cancel_background_task"), \
            f"Expected cancel. Got: {result.tool_names}"
        # Strict: fg file ops must happen WHILE bg tasks are running (true concurrency)
        assert_fg_during_bg(result, min_fg=2)
        assert not result.has_non_cancel_errors, \
            f"Unexpected errors: {[e.output[:200] for e in result.non_cancel_error_events]}"


# ===========================================================================
# Test 4: High-volume foreground burst with background running
# ===========================================================================


@pytest.mark.skipif(not EvalAgent.has_all(), reason="API + Daytona both required")
class TestHighVolumeForegroundBurst:
    """Many rapid foreground operations while background tasks run."""

    @pytest.fixture(scope="class")
    def sandbox(self):
        sb = create_test_sandbox("bg-burst")
        yield sb
        delete_test_sandbox(sb["id"])

    @pytest.mark.asyncio
    async def test_ten_foreground_ops_with_two_background(self, sandbox):
        """2 background tasks + 10 foreground echo commands."""
        agent = create_eval_agent(
            system_prompt=AGENT_PROMPT,
            sandbox_id=sandbox["id"],
            enable_background_tasks=True,
        )
        fg_steps = "\n".join(
            f"{i+3}. Run 'echo FG_STEP_{i+1}' in foreground"
            for i in range(10)
        )
        result = await agent.invoke(
            "Execute ALL of these steps:\n"
            "1. Run 'sleep 30 && echo BG_BUILD_DONE' in background (background: true)\n"
            "2. Run 'sleep 45 && echo BG_TEST_DONE' in background (background: true)\n"
            f"{fg_steps}\n"
            "13. Check background progress using check_background_progress\n"
            "14. Cancel all background tasks\n\n"
            "Use background: true for steps 1-2 ONLY. Execute each step with daytona_shell."
        )
        log_result(result, "burst")

        # Strict: 2 background launches with background: true
        bg_bash = [tc for tc in result.tool_calls
                   if tc.name == "daytona_shell" and tc.input.get("background") is True]
        assert len(bg_bash) >= 2, \
            f"Expected 2+ daytona_shell with background: true. Got {len(bg_bash)}"
        assert len(result.background_started()) >= 2, \
            f"Expected 2+ BackgroundTaskStarted events. Got {len(result.background_started())}"
        # Strict: at least 8 foreground bash calls (10 requested, allow some merging)
        fg_bash = [tc for tc in result.tool_calls
                   if tc.name == "daytona_shell" and not tc.input.get("background")]
        assert len(fg_bash) >= 8, \
            f"Expected 8+ foreground bash calls for 10 echo steps. Got {len(fg_bash)}"
        # Strict: total tool count must reflect full workload
        # Background launches appear as BackgroundTaskStarted, not ToolExecutionStarted
        # Foreground total: 8+ fg bash + 1 check + 1 cancel = 10+
        assert len(result.tools_started()) >= 10, \
            f"Expected 10+ foreground tool calls. Got {len(result.tools_started())}: {result.tool_names}"
        assert len(result.tools_started()) + len(result.background_started()) >= 12, \
            f"Expected 12+ total actions (fg + bg). Got {len(result.tools_started())} fg + {len(result.background_started())} bg"
        assert result.has_tool("check_background_progress"), \
            f"Expected progress check. Got: {result.tool_names}"
        assert result.has_tool("cancel_background_task"), \
            f"Expected cancel. Got: {result.tool_names}"
        # Strict: fg burst must happen WHILE bg tasks are running (true concurrency)
        assert_fg_during_bg(result, min_fg=5)
        assert not result.has_non_cancel_errors, \
            f"Errors under high concurrency: {[e.output[:200] for e in result.non_cancel_error_events]}"


# ===========================================================================
# Test 5: Four background tasks — max concurrency stress
# ===========================================================================


@pytest.mark.skipif(not EvalAgent.has_all(), reason="API + Daytona both required")
class TestFourBackgroundMaxConcurrency:
    """Push concurrency limits with 4 background + 3 foreground tasks."""

    @pytest.fixture(scope="class")
    def sandbox(self):
        sb = create_test_sandbox("bg-max-concur")
        yield sb
        delete_test_sandbox(sb["id"])

    @pytest.mark.asyncio
    async def test_four_bg_three_fg(self, sandbox):
        """Launch 4 bg tasks, do 3 fg tasks, check all, cancel all."""
        agent = create_eval_agent(
            system_prompt=AGENT_PROMPT,
            sandbox_id=sandbox["id"],
            enable_background_tasks=True,
        )
        result = await agent.invoke(
            "Execute these steps:\n"
            "1. Run 'sleep 10 && echo LINT_DONE' in background (background: true)\n"
            "2. Run 'sleep 15 && echo UNIT_DONE' in background (background: true)\n"
            "3. Run 'sleep 20 && echo INTEG_DONE' in background (background: true)\n"
            "4. Run 'sleep 30 && echo E2E_DONE' in background (background: true)\n"
            "5. Run 'echo DEPLOYING_CONFIG' in foreground\n"
            "6. Create /home/daytona/deploy.log with 'deploy started' using daytona_write_file\n"
            "7. Run 'echo MIGRATION_DONE' in foreground\n"
            "8. Check all background task progress using check_background_progress\n"
            "9. Cancel ALL remaining background tasks\n"
            "10. Report how many background tasks were running\n\n"
            "Use background: true for steps 1-4 ONLY."
        )
        log_result(result, "max_concurrency")

        # Strict: exactly 4 background launches with background: true
        bg_bash = [tc for tc in result.tool_calls
                   if tc.name == "daytona_shell" and tc.input.get("background") is True]
        assert len(bg_bash) >= 4, \
            f"Expected 4+ daytona_shell with background: true. Got {len(bg_bash)}"
        assert len(result.background_started()) >= 4, \
            f"Expected 4+ BackgroundTaskStarted events. Got {len(result.background_started())}"
        # Strict: foreground work must include bash + write_file
        fg_bash = [tc for tc in result.tool_calls
                   if tc.name == "daytona_shell" and not tc.input.get("background")]
        assert len(fg_bash) >= 2, \
            f"Expected 2+ foreground bash calls. Got {len(fg_bash)}"
        assert result.has_tool("daytona_write_file"), \
            f"Expected daytona_write_file for deploy.log. Got: {result.tool_names}"
        # Strict: deploy.log file path verification
        write_calls = [tc for tc in result.tool_calls if tc.name == "daytona_write_file"]
        assert any("deploy" in tc.input.get("file_path", "") for tc in write_calls), \
            f"Expected deploy.log write. Got: {[tc.input for tc in write_calls]}"
        assert result.has_tool("check_background_progress"), \
            f"Expected progress check. Got: {result.tool_names}"
        assert result.has_tool("cancel_background_task"), \
            f"Expected cancel. Got: {result.tool_names}"
        # Background launches appear as BackgroundTaskStarted, not ToolExecutionStarted.
        # Foreground total: 2 fg bash + 1 write + 1 check + cancels >= 6
        assert len(result.tools_started()) >= 6, \
            f"Expected 6+ foreground tool calls. Got {len(result.tools_started())}"
        assert len(result.tools_started()) + len(result.background_started()) >= 9, \
            f"Expected 9+ total actions (fg + bg). Got {len(result.tools_started())} fg + {len(result.background_started())} bg"
        # Strict: fg calls must happen WHILE bg tasks are running (true concurrency)
        assert_fg_during_bg(result, min_fg=2)
        assert not result.has_non_cancel_errors, \
            f"Errors under max concurrency: {[e.output[:200] for e in result.non_cancel_error_events]}"
