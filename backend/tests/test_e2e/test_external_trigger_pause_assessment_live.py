# ruff: noqa
"""Live e2e tests — External trigger: pause assessment via assess_pause().

Tests the full pause assessment flow through the real LLM:

  1. Affected task (imports from broken file) → YES verdict
  2. Unaffected task (no dependency on broken file) → NO verdict
  3. Ambiguous case — task reads but doesn't import broken file → correct verdict
  4. Multiple broken files — task depends on one of them → YES
  5. Verdict conversation trail preserves original snapshot + blocker Q + answer
  6. Pydantic validation — answer is always "YES" or "NO" (Literal type)

Uses assess_pause() which wraps run_external_trigger() → runner.run().

Run with:
    .venv/bin/python -m pytest backend/tests/test_e2e/test_external_trigger_pause_assessment_live.py -v -m live -o "addopts="
"""

from __future__ import annotations

import pytest

from engine.testing.eval_agent import EvalAgent
from external_trigger.pause_assessment import assess_pause
from tests.test_e2e.conftest import create_eval_agent

pytestmark = [pytest.mark.e2e, pytest.mark.live]

HAS_CREDENTIALS = EvalAgent.has_credentials()


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------


@pytest.fixture(scope="module")
def agent():
    if not HAS_CREDENTIALS:
        pytest.skip("No LLM credentials configured")
    return create_eval_agent()


@pytest.fixture(scope="module")
def api_client(agent):
    return agent.api_client


# ---------------------------------------------------------------------------
# Test 1: Affected task → YES
# ---------------------------------------------------------------------------


@pytest.mark.skipif(not HAS_CREDENTIALS, reason="No credentials")
@pytest.mark.asyncio
async def test_affected_task_says_yes(api_client):
    """Task that imports from the broken file should answer YES."""
    messages = [
        {"role": "user", "content": (
            "Fix the IO module exports in pkg/io.py. "
            "The module imports load_defaults from pkg/_compat.py on line 3."
        )},
        {"role": "assistant", "content": (
            "I've read pkg/io.py. The file imports `load_defaults` from `pkg._compat` "
            "on line 3, which is central to the IO module's initialization flow."
        )},
    ]

    verdict = await assess_pause(
        task_id="fix-io",
        agent_run_id="run-fix-io",
        messages=messages,
        system_prompt="You are a developer agent working on pkg/io.py.",
        broken_files=["pkg/_compat.py"],
        problem="pkg/_compat.py was refactored — load_defaults() renamed to get_defaults(). All importers will fail.",
        api_client=api_client,
    )

    assert verdict.answer == "YES", f"Expected YES, got {verdict.answer}: {verdict.reason}"
    assert len(verdict.reason) > 0, "Verdict should include a reason"
    assert verdict.task_id == "fix-io"
    assert verdict.turns_used >= 1


# ---------------------------------------------------------------------------
# Test 2: Unaffected task → NO
# ---------------------------------------------------------------------------


@pytest.mark.skipif(not HAS_CREDENTIALS, reason="No credentials")
@pytest.mark.asyncio
async def test_unaffected_task_says_no(api_client):
    """Task with no dependency on broken files should answer NO."""
    messages = [
        {"role": "user", "content": (
            "Fix the CLI entry points in pkg/cli.py. "
            "The CLI uses argparse to parse commands."
        )},
        {"role": "assistant", "content": (
            "I've read pkg/cli.py. The CLI module uses argparse and "
            "only imports from the standard library and pkg.commands. "
            "It has no dependency on the database layer."
        )},
    ]

    verdict = await assess_pause(
        task_id="fix-cli",
        agent_run_id="run-fix-cli",
        messages=messages,
        system_prompt="You are a developer agent working on pkg/cli.py.",
        broken_files=["src/db/connection.py", "src/db/pool.py"],
        problem="Database connection pool has a deadlock bug. All DB queries will hang.",
        api_client=api_client,
    )

    assert verdict.answer == "NO", f"Expected NO, got {verdict.answer}: {verdict.reason}"
    assert verdict.task_id == "fix-cli"


# ---------------------------------------------------------------------------
# Test 3: Multiple broken files — task depends on one → YES
# ---------------------------------------------------------------------------


@pytest.mark.skipif(not HAS_CREDENTIALS, reason="No credentials")
@pytest.mark.asyncio
async def test_multiple_broken_files_partial_dependency_yes(api_client):
    """Task depends on one of several broken files → YES."""
    messages = [
        {"role": "user", "content": (
            "Fix the event processor in src/events/processor.py. "
            "The processor imports validate_payload from src/schema.py "
            "and uses DateTimeField for timestamp parsing."
        )},
        {"role": "assistant", "content": (
            "I've read src/events/processor.py. It imports validate_payload "
            "from src/schema.py on line 5 and uses DateTimeField for all "
            "incoming event timestamp validation."
        )},
    ]

    verdict = await assess_pause(
        task_id="fix-events",
        agent_run_id="run-fix-events",
        messages=messages,
        system_prompt="You are a developer agent working on src/events/processor.py.",
        broken_files=["src/schema.py", "src/validators.py", "src/serializers.py"],
        problem=(
            "src/schema.py DateTimeField changed from auto-coercing to strict mode. "
            "src/validators.py removed backward compat shim. "
            "src/serializers.py output format changed."
        ),
        api_client=api_client,
    )

    assert verdict.answer == "YES", f"Expected YES (depends on schema.py), got {verdict.answer}: {verdict.reason}"


# ---------------------------------------------------------------------------
# Test 4: Conversation trail preserves snapshot + blocker Q + answer
# ---------------------------------------------------------------------------


@pytest.mark.skipif(not HAS_CREDENTIALS, reason="No credentials")
@pytest.mark.asyncio
async def test_conversation_trail_extends_snapshot(api_client):
    """Verdict conversation should be longer than the original snapshot."""
    original_messages = [
        {"role": "user", "content": "Fix pkg/parser.py. It imports from pkg._compat."},
        {"role": "assistant", "content": "Reading pkg/parser.py. It uses load_defaults from pkg._compat."},
    ]

    verdict = await assess_pause(
        task_id="fix-parser",
        agent_run_id="run-fix-parser",
        messages=original_messages,
        system_prompt="You are a developer agent working on pkg/parser.py.",
        broken_files=["pkg/_compat.py"],
        problem="pkg/_compat.py refactored — load_defaults renamed.",
        api_client=api_client,
    )

    # Conversation should contain: original snapshot + blocker Q (user) + answer (assistant with tool_use)
    assert len(verdict.conversation) > len(original_messages), (
        f"Verdict conversation ({len(verdict.conversation)} msgs) should extend "
        f"original snapshot ({len(original_messages)} msgs)"
    )


# ---------------------------------------------------------------------------
# Test 5: Pydantic validation — answer is Literal["YES", "NO"]
# ---------------------------------------------------------------------------


@pytest.mark.skipif(not HAS_CREDENTIALS, reason="No credentials")
@pytest.mark.asyncio
async def test_verdict_answer_is_valid_literal(api_client):
    """Answer must be exactly "YES" or "NO" after Pydantic validation."""
    messages = [
        {"role": "user", "content": "Fix src/utils.py. Pure utility functions, no external deps."},
        {"role": "assistant", "content": "Reading src/utils.py. Only imports math and os. No project deps."},
    ]

    verdict = await assess_pause(
        task_id="fix-utils",
        agent_run_id="run-fix-utils",
        messages=messages,
        system_prompt="You are a developer agent working on src/utils.py.",
        broken_files=["src/db/models.py"],
        problem="Database model schema changed.",
        api_client=api_client,
    )

    assert verdict.answer in ("YES", "NO"), f"Answer must be YES or NO, got: {verdict.answer}"
    # This task has no DB dependency, so expect NO
    assert verdict.answer == "NO", f"Utils has no DB dependency, expected NO: {verdict.reason}"


# ---------------------------------------------------------------------------
# Test 6: Empty conversation snapshot — still produces valid verdict
# ---------------------------------------------------------------------------


@pytest.mark.skipif(not HAS_CREDENTIALS, reason="No credentials")
@pytest.mark.asyncio
async def test_empty_snapshot_still_produces_verdict(api_client):
    """Even with no prior conversation, the assessment should produce a valid verdict."""
    verdict = await assess_pause(
        task_id="new-task",
        agent_run_id="run-new-task",
        messages=[],
        system_prompt="You are a developer agent. You have not started any work yet.",
        broken_files=["src/config.py"],
        problem="Config module restructured.",
        api_client=api_client,
    )

    assert verdict.answer in ("YES", "NO"), f"Must produce valid verdict, got: {verdict.answer}"
    assert verdict.turns_used >= 1
