# ruff: noqa
"""Live E2E: Physical process cancellation — verify cancelled bg tasks are killed.

Tests that cancel_background_task actually kills the sandbox process,
not just logically marks it as cancelled.

Run with: .venv/bin/python -m pytest backend/tests/test_e2e/test_bg_physical_cancel_live.py -v -s --log-cli-level=INFO
"""

from __future__ import annotations

import pytest

from engine.testing.eval_agent import EvalAgent
from tests.test_e2e.conftest import create_eval_agent, create_test_sandbox, delete_test_sandbox
from tests.test_e2e.helpers import log_result

pytestmark = [pytest.mark.e2e, pytest.mark.live]

AGENT_PROMPT = """\
You are test-cancel-agent, a developer with a remote Daytona sandbox.

IMPORTANT RULES:
- You MUST use tools for every action — never just describe what you'd do.
- Use daytona_shell to run commands, daytona_write_file to create files.
- You have background task support: add "background": true to tool input for long-running operations.
- Use check_background_progress to monitor background tasks.
- Use cancel_background_task to cancel running background tasks.

Always be concise. Execute tools, don't just describe them.
"""


# ===========================================================================
# Test 1: Cancel kills the process — marker file is NOT created
# ===========================================================================


@pytest.mark.skipif(not EvalAgent.has_all(), reason="API + Daytona both required")
class TestPhysicalCancelKillsProcess:
    """After cancel, the background process should be dead — not still running."""

    @pytest.fixture(scope="class")
    def sandbox(self):
        sb = create_test_sandbox("cancel-kill")
        yield sb
        delete_test_sandbox(sb["id"])

    @pytest.mark.asyncio
    async def test_cancelled_process_does_not_complete(self, sandbox):
        """Launch a bg task that writes a marker file after sleeping.

        Cancel the task, then verify the marker file was never created
        — proving the process was physically killed, not just logically
        marked as cancelled.
        """
        agent = create_eval_agent(
            system_prompt=AGENT_PROMPT,
            sandbox_id=sandbox["id"],
            enable_background_tasks=True,
        )
        result = await agent.invoke(
            "Do these steps IN ORDER:\n"
            "1. Run this command in background (background: true):\n"
            "   sleep 15 && echo MARKER > /tmp/cancel_test_marker.txt\n"
            "2. Run 'echo READY' in foreground\n"
            "3. Check background progress using check_background_progress\n"
            "4. Cancel the background task using cancel_background_task "
            "with reason: 'Testing physical cancel'\n"
            "5. Wait 3 seconds: run 'sleep 3 && echo WAITED' in foreground\n"
            "6. Check if the marker file exists: run "
            "'cat /tmp/cancel_test_marker.txt 2>&1 || echo FILE_NOT_FOUND' in foreground\n"
            "7. Report the result of step 6\n\n"
            "Use background: true for step 1 ONLY."
        )
        log_result(result, "physical_cancel")

        # --- Assertions ---

        # 1. Background task was launched
        assert result.has_tool_with_background("daytona_shell"), \
            f"Expected daytona_shell with background: true. Got: {result.tool_calls}"
        assert len(result.background_started()) >= 1, \
            f"Expected BackgroundTaskStarted event. Got: {result.tool_names}"

        # 2. Cancel was issued
        cancel_calls = [tc for tc in result.tool_calls if tc.name == "cancel_background_task"]
        assert len(cancel_calls) >= 1, \
            f"Expected cancel_background_task call. Got: {result.tool_names}"

        # 3. The marker file should NOT exist — proving the process was killed
        # Look for the cat command output in the final text or tool results
        text_lower = result.text.lower()
        has_file_not_found = any(
            w in text_lower for w in ["file_not_found", "no such file", "not found", "does not exist"]
        )
        # Also check tool outputs for the verification command
        verify_outputs = []
        for evt in result.tools_completed():
            if "cancel_test_marker" in evt.output or "FILE_NOT_FOUND" in evt.output:
                verify_outputs.append(evt.output)

        marker_was_created = any("MARKER" in o and "FILE_NOT_FOUND" not in o for o in verify_outputs)

        assert not marker_was_created, (
            f"Marker file was created — process was NOT physically killed! "
            f"Verify outputs: {verify_outputs}"
        )
        assert has_file_not_found or any("FILE_NOT_FOUND" in o for o in verify_outputs), (
            f"Expected FILE_NOT_FOUND in output — proving process was killed. "
            f"Text: {result.text[:300]}, verify outputs: {verify_outputs}"
        )

        assert not result.has_non_cancel_errors, \
            f"Unexpected errors: {[e.output[:200] for e in result.non_cancel_error_events]}"


# ===========================================================================
# Test 2: Cancel kills the process — PID is gone
# ===========================================================================


@pytest.mark.skipif(not EvalAgent.has_all(), reason="API + Daytona both required")
class TestPhysicalCancelPidGone:
    """After cancel, the process PID should not exist in the process table."""

    @pytest.fixture(scope="class")
    def sandbox(self):
        sb = create_test_sandbox("cancel-pid")
        yield sb
        delete_test_sandbox(sb["id"])

    @pytest.mark.asyncio
    async def test_cancelled_pid_not_in_process_table(self, sandbox):
        """Launch a bg sleep, cancel it, then verify no sleep process remains."""
        agent = create_eval_agent(
            system_prompt=AGENT_PROMPT,
            sandbox_id=sandbox["id"],
            enable_background_tasks=True,
        )
        result = await agent.invoke(
            "Do these steps IN ORDER:\n"
            "1. Run 'sleep 300' in background (background: true)\n"
            "2. Run 'echo FOREGROUND_OK' in foreground\n"
            "3. Check background progress using check_background_progress\n"
            "4. Cancel the background task using cancel_background_task "
            "with reason: 'PID test cancel'\n"
            "5. Wait briefly: run 'sleep 2 && echo WAIT_DONE' in foreground\n"
            "6. Check if sleep 300 is still running: run "
            "'pgrep -f \"sleep 300\" && echo STILL_RUNNING || echo PROCESS_GONE' in foreground\n"
            "7. Report what step 6 showed\n\n"
            "Use background: true for step 1 ONLY."
        )
        log_result(result, "pid_gone")

        # --- Assertions ---

        # 1. Background task was launched and cancelled
        assert result.has_tool_with_background("daytona_shell"), \
            f"Expected daytona_shell with background: true. Got: {result.tool_calls}"
        cancel_calls = [tc for tc in result.tool_calls if tc.name == "cancel_background_task"]
        assert len(cancel_calls) >= 1, \
            f"Expected cancel_background_task. Got: {result.tool_names}"

        # 2. The sleep 300 process should be gone
        text_lower = result.text.lower()
        verify_outputs = []
        for evt in result.tools_completed():
            if "PROCESS_GONE" in evt.output or "STILL_RUNNING" in evt.output:
                verify_outputs.append(evt.output)

        process_still_running = any("STILL_RUNNING" in o for o in verify_outputs)
        process_gone = any("PROCESS_GONE" in o for o in verify_outputs)

        assert not process_still_running, (
            f"sleep 300 is STILL RUNNING after cancel — physical kill failed! "
            f"Verify outputs: {verify_outputs}"
        )
        assert process_gone or "process_gone" in text_lower, (
            f"Expected PROCESS_GONE — proving PID was killed. "
            f"Text: {result.text[:300]}, verify outputs: {verify_outputs}"
        )

        assert not result.has_non_cancel_errors, \
            f"Unexpected errors: {[e.output[:200] for e in result.non_cancel_error_events]}"
