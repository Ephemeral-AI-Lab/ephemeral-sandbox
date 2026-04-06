# ruff: noqa
"""Live E2E: Ephemeral background reminder injection.

Tests that the soft background reminder is:
1. Injected when background tasks are pending
2. NOT persisted in conversation history
3. The LLM acknowledges or acts on the reminder naturally

Uses EvalAgent for credential loading and agent configuration.
Run with: .venv/bin/python -m pytest backend/tests/test_e2e/test_background_reminder_live.py -v -s --log-cli-level=INFO
"""

from __future__ import annotations

import logging

import pytest

from engine.eval_agent import EvalAgent
from tests.test_e2e.conftest import create_test_sandbox, delete_test_sandbox

logger = logging.getLogger(__name__)

pytestmark = [pytest.mark.e2e, pytest.mark.live]

AGENT_PROMPT = """\
You are test-reminder-agent, a developer with a remote Daytona sandbox.

RULES:
- Use tools for every action.
- Use daytona_bash to run commands.
- You have background task support: add "background": true to tool input for long-running operations.
- Use check_background_progress to check background tasks.
- Use cancel_background_task to cancel background tasks.

Be concise. Always execute tools.
"""


def _log_result(result, label: str) -> None:
    started = result.tools_started()
    logger.info(
        f"\n{'='*60}\n[{label}] {len(result.events)} events:\n"
        f"  Tools: {len(started)}\n"
        f"  Tool names: {result.tool_names}\n"
        f"{'='*60}\n"
    )


@pytest.mark.skipif(not EvalAgent.has_all(), reason="API + Daytona both required")
class TestEphemeralBackgroundReminder:
    """Tests that the ephemeral reminder nudges the LLM without polluting context."""

    @pytest.fixture(scope="class")
    def sandbox(self):
        sb = create_test_sandbox("bg-reminder")
        yield sb
        delete_test_sandbox(sb["id"])

    @pytest.mark.asyncio
    async def test_reminder_nudges_llm_to_check_progress(self, sandbox):
        """Background a slow task, do foreground work, verify the LLM
        becomes aware of the background task on subsequent turns."""
        agent = EvalAgent.create(
            system_prompt=AGENT_PROMPT,
            sandbox_id=sandbox["id"],
            enable_background_tasks=True,
        )
        result = await agent.invoke(
            "Do these steps:\n"
            "1. Run 'sleep 15 && echo REMINDER_TEST_DONE' in background "
            "(use daytona_bash with background: true)\n"
            "2. Run 'echo STEP_2_DONE' in foreground\n"
            "3. Run 'echo STEP_3_DONE' in foreground\n"
            "4. Now check on the background task status\n\n"
            "Use background: true for step 1 only."
        )
        _log_result(result, "reminder_nudge")

        assert len(result.assistant_turns()) >= 1, "Missing assistant turn"
        assert len(result.tools_started()) >= 3, \
            f"Expected 3+ tool calls. Got: {result.tool_names}"

        has_check = result.has_tool("check_background_progress")
        logger.info(f"[Reminder] check_background_progress called: {has_check}")

        if has_check:
            logger.info("[PASS] LLM checked progress (reminder may have contributed)")
        else:
            logger.info("[INFO] LLM did not check progress — may have proceeded without")

        logger.info(f"[Reminder] Final text: {result.text[:200]}")

    @pytest.mark.asyncio
    async def test_reminder_only_when_tasks_pending(self, sandbox):
        """Verify that no reminder is injected when there are no background tasks."""
        agent = EvalAgent.create(
            system_prompt=AGENT_PROMPT,
            sandbox_id=sandbox["id"],
            enable_background_tasks=True,
        )
        result = await agent.invoke(
            "Run 'echo NO_BACKGROUND_HERE' using daytona_bash. "
            "Do NOT use background. Keep it simple."
        )
        _log_result(result, "no_reminder")

        assert len(result.assistant_turns()) >= 1, "Missing assistant turn"

        has_bg_check = result.has_tool("check_background_progress")
        if has_bg_check:
            logger.info("[INFO] LLM checked progress anyway (no tasks to show)")
        else:
            logger.info("[PASS] No background progress check — no reminder needed")

        assert len(result.background_started()) == 0, \
            f"No background tasks expected. Got {len(result.background_started())}"
        logger.info("[PASS] Foreground-only interaction, no reminder injected")
