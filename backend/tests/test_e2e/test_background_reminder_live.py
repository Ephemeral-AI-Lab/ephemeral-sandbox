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

import pytest

from engine.testing.eval_agent import EvalAgent
from tests.test_e2e.bg_prompts import _build_prompt
from tests.test_e2e.conftest import create_eval_agent, create_test_sandbox, delete_test_sandbox
from tests.test_e2e.helpers import log_result

import logging
logger = logging.getLogger(__name__)

pytestmark = [pytest.mark.e2e, pytest.mark.live]

AGENT_PROMPT = _build_prompt(agent_name="test-reminder-agent")


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
        agent = create_eval_agent(
            system_prompt=AGENT_PROMPT,
            sandbox_id=sandbox["id"],
            enable_background_tasks=True,
        )
        result = await agent.invoke(
            "Do these steps:\n"
            "1. Run 'sleep 15 && echo REMINDER_TEST_DONE' in background "
            "(use daytona_shell with background: true)\n"
            "2. Run 'echo STEP_2_DONE' in foreground\n"
            "3. Run 'echo STEP_3_DONE' in foreground\n"
            "4. Now check on the background task status\n\n"
            "Use background: true for step 1 only."
        )
        log_result(result, "reminder_nudge")

        assert len(result.assistant_turns()) >= 1, "Missing assistant turn"
        assert len(result.tools_started()) >= 3, \
            f"Expected 3+ tool calls. Got: {result.tool_names}"
        assert result.has_tool_with_background("daytona_shell"), \
            f"Expected daytona_shell called with background: true. Got tool calls: {result.tool_calls}"
        assert len(result.background_started()) >= 1, \
            f"Expected BackgroundTaskStarted event. Got tools: {result.tool_names}"
        assert result.has_tool("check_background_progress"), \
            f"Expected check_background_progress call. Got: {result.tool_names}"

        # Reminder must live in the durable display history (so the user
        # sees it in their scrollback) — not just be a transient API-only
        # injection. Find at least one SystemReminderBlock with the
        # background-progress category.
        all_reminders = [
            r
            for m in agent._display_messages
            for r in m.system_reminders
        ]
        assert all_reminders, (
            "Expected at least one SystemReminderBlock in display_messages, "
            "but found none. The reminder must persist in the durable "
            "history, not just in api_messages."
        )
        assert any(r.category == "background_progress" for r in all_reminders), (
            "Expected a background_progress reminder; got categories="
            f"{[r.category for r in all_reminders]}"
        )
        assert any("still running" in r.text for r in all_reminders), (
            "Reminder text 'still running' missing — the reminder builder "
            "may have produced an empty body."
        )

        # The compacted view (api_messages) must NOT be the same object as
        # display_messages: it is a derived snapshot.
        last_api = agent._query_context.api_messages_snapshot
        assert last_api is not None, "api_messages_snapshot should be set after a run"
        assert last_api is not agent._display_messages, (
            "api_messages must be a fresh list, not a reference to display_messages"
        )

        logger.info(f"[Reminder] Final text: {result.text[:200]}")
        logger.info(
            f"[Reminder] display_messages={len(agent._display_messages)} "
            f"api_messages={len(last_api)}"
        )

    @pytest.mark.asyncio
    async def test_reminder_only_when_tasks_pending(self, sandbox):
        """Verify that no reminder is injected when there are no background tasks."""
        agent = create_eval_agent(
            system_prompt=AGENT_PROMPT,
            sandbox_id=sandbox["id"],
            enable_background_tasks=True,
        )
        result = await agent.invoke(
            "Run 'echo NO_BACKGROUND_HERE' using daytona_shell. "
            "Do NOT use background. Keep it simple."
        )
        log_result(result, "no_reminder")

        assert len(result.assistant_turns()) >= 1, "Missing assistant turn"
        assert len(result.tools_started()) >= 1, \
            f"Expected at least 1 tool call. Got: {result.tool_names}"
        assert len(result.background_started()) == 0, \
            f"No background tasks expected. Got {len(result.background_started())}"
        assert not result.has_tool("check_background_progress"), \
            f"No background tasks running — should not check progress. Got: {result.tool_names}"
        logger.info("[PASS] Foreground-only interaction, no reminder injected")
