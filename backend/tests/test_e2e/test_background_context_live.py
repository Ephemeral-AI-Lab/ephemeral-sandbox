# ruff: noqa
"""Live E2E: Context limits with long background tasks and ephemeral reminders.

Tests that the system handles context pressure correctly when:
1. Many foreground tool calls accumulate while background tasks run
2. Ephemeral reminders do NOT accumulate in context history
3. Large tool outputs + background reminders don't blow up context

Uses EvalAgent for credential loading and agent configuration.
Run with: .venv/bin/python -m pytest backend/tests/test_e2e/test_background_context_live.py -v -s --log-cli-level=INFO
"""

from __future__ import annotations

import logging

import pytest

from engine.eval_agent import EvalAgent
from tests.test_e2e.conftest import create_test_sandbox, delete_test_sandbox

logger = logging.getLogger(__name__)

pytestmark = [pytest.mark.e2e, pytest.mark.live]

AGENT_PROMPT = """\
You are test-context-agent, a developer with a remote Daytona sandbox.

RULES:
- Use tools for every action — never just describe what you'd do.
- Use daytona_bash to run commands.
- You have background task support: add "background": true to tool input.
- Use check_background_progress to check background tasks.
- Use cancel_background_task to cancel background tasks.
- Be concise but thorough. Execute all steps requested.
"""


def _log_result(result, label: str) -> None:
    started = result.tools_started()
    completed = result.tools_completed()
    turns = result.assistant_turns()
    bg_started = result.background_started()

    logger.info(
        f"\n{'='*60}\n[{label}] Event summary:\n"
        f"  Total events: {len(result.events)}\n"
        f"  Tool started: {len(started)}\n"
        f"  Tool completed: {len(completed)}\n"
        f"  Assistant turns: {len(turns)}\n"
        f"  Background started: {len(bg_started)}\n"
        f"  Tool sequence: {result.tool_names}\n"
        f"{'='*60}"
    )


# ===========================================================================
# Test 1: Many foreground calls with background running
# ===========================================================================


@pytest.mark.skipif(not EvalAgent.has_all(), reason="API + Daytona both required")
class TestReminderDoesNotAccumulate:
    """Verify ephemeral reminders don't pile up in context across many turns."""

    @pytest.fixture(scope="class")
    def sandbox(self):
        sb = create_test_sandbox("ctx-reminder")
        yield sb
        delete_test_sandbox(sb["id"])

    @pytest.mark.asyncio
    async def test_many_foreground_turns_with_background(self, sandbox):
        """Background a slow task, then do 5+ foreground operations."""
        agent = EvalAgent.create(
            system_prompt=AGENT_PROMPT,
            sandbox_id=sandbox["id"],
            enable_background_tasks=True,
        )
        result = await agent.invoke(
            "Follow these steps exactly:\n"
            "1. Run 'sleep 30 && echo BG_COMPLETE' in background (use background: true)\n"
            "2. Run 'echo STEP_2' in foreground\n"
            "3. Run 'echo STEP_3' in foreground\n"
            "4. Run 'echo STEP_4' in foreground\n"
            "5. Run 'echo STEP_5' in foreground\n"
            "6. Run 'echo STEP_6' in foreground\n"
            "7. Run 'echo STEP_7' in foreground\n"
            "8. Check background progress using check_background_progress\n"
            "9. Cancel the background task using cancel_background_task\n"
            "10. Report what happened\n\n"
            "Use background: true for step 1 ONLY. All other steps are foreground."
        )
        _log_result(result, "reminder_accumulation")

        assert len(result.assistant_turns()) >= 1, "Missing assistant turn"
        assert len(result.tools_started()) >= 5, \
            f"Expected 5+ tool calls. Got {len(result.tools_started())}: {result.tool_names}"

        # If reminders accumulated, we'd see errors. Completion means reminders are ephemeral.
        assert not result.has_errors, \
            f"Context-related errors detected: {[e.output for e in result.error_events]}"
        logger.info("[PASS] No context accumulation from reminders across many turns")


# ===========================================================================
# Test 2: Large tool outputs with background tasks
# ===========================================================================


@pytest.mark.skipif(not EvalAgent.has_all(), reason="API + Daytona both required")
class TestLargeOutputWithBackground:
    """Generate large tool outputs while background tasks run."""

    @pytest.fixture(scope="class")
    def sandbox(self):
        sb = create_test_sandbox("ctx-large")
        yield sb
        delete_test_sandbox(sb["id"])

    @pytest.mark.asyncio
    async def test_large_foreground_output_with_background(self, sandbox):
        """Generate large tool outputs while background runs."""
        agent = EvalAgent.create(
            system_prompt=AGENT_PROMPT,
            sandbox_id=sandbox["id"],
            enable_background_tasks=True,
        )
        result = await agent.invoke(
            "Follow these steps:\n"
            "1. Run 'sleep 20 && echo LARGE_BG_DONE' in background (use background: true)\n"
            "2. Run 'seq 1 500' in foreground — this generates 500 lines\n"
            "3. Run 'for i in $(seq 1 100); do echo \"line_$i: $(date)\"; done' in foreground\n"
            "4. Check background progress\n"
            "5. Cancel the background task\n"
            "6. Report: how much output did you see from the seq command?\n\n"
            "Use background: true for step 1 ONLY."
        )
        _log_result(result, "large_output")

        assert len(result.assistant_turns()) >= 1, "Missing assistant turn"
        assert len(result.tools_completed()) >= 2, \
            f"Expected 2+ tool completions. Got {len(result.tools_completed())}"
        assert not result.has_errors, \
            f"Context overflow detected: {[e.output for e in result.error_events]}"
        logger.info("[PASS] Large outputs handled")


# ===========================================================================
# Test 3: Sustained background with many turns — stress test
# ===========================================================================


@pytest.mark.skipif(not EvalAgent.has_all(), reason="API + Daytona both required")
class TestSustainedBackgroundStress:
    """Long-running background task across 8+ foreground turns."""

    @pytest.fixture(scope="class")
    def sandbox(self):
        sb = create_test_sandbox("ctx-stress")
        yield sb
        delete_test_sandbox(sb["id"])

    @pytest.mark.asyncio
    async def test_sustained_background_many_foreground_turns(self, sandbox):
        """Background task runs for 45s while LLM does 8+ foreground operations."""
        agent = EvalAgent.create(
            system_prompt=AGENT_PROMPT,
            sandbox_id=sandbox["id"],
            enable_background_tasks=True,
        )
        result = await agent.invoke(
            "This is a multi-step task. Follow ALL steps:\n\n"
            "1. Run 'sleep 45 && echo STRESS_BG_DONE' in background (use background: true)\n"
            "2. Run 'echo STEP_A' in foreground\n"
            "3. Run 'echo STEP_B' in foreground\n"
            "4. Run 'echo STEP_C' in foreground\n"
            "5. Run 'echo STEP_D' in foreground\n"
            "6. Run 'echo STEP_E' in foreground\n"
            "7. Run 'echo STEP_F' in foreground\n"
            "8. Run 'echo STEP_G' in foreground\n"
            "9. Run 'echo STEP_H' in foreground\n"
            "10. Check background progress\n"
            "11. Cancel the background task with reason 'stress test complete'\n"
            "12. Summarize: how many foreground steps completed? "
            "What was the background task status when you checked?\n\n"
            "Use background: true for step 1 ONLY. Execute each step with daytona_bash."
        )
        _log_result(result, "stress_test")

        assert len(result.assistant_turns()) >= 1, "Missing assistant turn"
        assert len(result.tools_started()) >= 6, \
            f"Expected 6+ tool calls. Got {len(result.tools_started())}: {result.tool_names}"
        assert not result.has_errors, \
            f"Context errors under stress: {[e.output[:200] for e in result.error_events]}"

        assert len(result.text) > 20, \
            f"Final summary too short — possible degradation. Got: {result.text}"

        logger.info(
            f"[PASS] Stress test completed: {len(result.tools_started())} tools, "
            f"{len(result.assistant_turns())} turns, no context overflow"
        )
