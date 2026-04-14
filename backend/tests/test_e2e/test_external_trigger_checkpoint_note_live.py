# ruff: noqa
"""Live e2e tests — External trigger: checkpoint note via run_checkpoint_note().

Tests the checkpoint note generation flow through the real LLM:

  1. Edit checkpoint — agent made file edits, note should mention files/changes
  2. Turn checkpoint — agent is mid-work, note should report status
  3. Stuck agent — agent is blocked, note should reflect that
  4. Empty snapshot — still produces a valid note
  5. Note content is meaningful (not just a placeholder)

Uses run_checkpoint_note() which wraps run_external_trigger() → runner.run().

Run with:
    .venv/bin/python -m pytest backend/tests/test_e2e/test_external_trigger_checkpoint_note_live.py -v -m live -o "addopts="
"""

from __future__ import annotations

import pytest

from engine.testing.eval_agent import EvalAgent
from external_trigger.tc_note import (
    EDIT_CHECKPOINT_PROMPT,
    TURN_CHECKPOINT_PROMPT,
    run_checkpoint_note,
)
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
# Test 1: Edit checkpoint — agent made file edits
# ---------------------------------------------------------------------------


@pytest.mark.skipif(not HAS_CREDENTIALS, reason="No credentials")
@pytest.mark.asyncio
async def test_edit_checkpoint_mentions_files(api_client):
    """Edit checkpoint note should mention the files that were edited."""
    messages = [
        {"role": "user", "content": (
            "Fix the broken import in pkg/io.py. Change load_defaults to get_defaults."
        )},
        {"role": "assistant", "content": (
            "I've read pkg/io.py. Line 3 has `from pkg._compat import load_defaults`. "
            "I'll change it to `from pkg._compat import get_defaults`."
        )},
        {"role": "user", "content": "Tool result: file edited successfully."},
        {"role": "assistant", "content": (
            "Done. I've edited pkg/io.py line 3: changed the import from "
            "`load_defaults` to `get_defaults`. Running pytest pkg/tests/test_io.py "
            "to verify... All 12 tests pass."
        )},
    ]

    result = await run_checkpoint_note(
        task_id="fix-io",
        agent_run_id="run-fix-io",
        messages=messages,
        prompt=EDIT_CHECKPOINT_PROMPT,
        trigger="edit",
        api_client=api_client,
    )

    assert result.task_id == "fix-io"
    assert result.trigger == "edit"
    assert len(result.note_summary) > 20, f"Note too short: {result.note_summary}"
    # Should mention the edited file
    note_lower = result.note_summary.lower()
    assert "io" in note_lower or "pkg" in note_lower, (
        f"Edit note should mention the file: {result.note_summary}"
    )
    assert result.turns_used >= 1


# ---------------------------------------------------------------------------
# Test 2: Turn checkpoint — agent is mid-work
# ---------------------------------------------------------------------------


@pytest.mark.skipif(not HAS_CREDENTIALS, reason="No credentials")
@pytest.mark.asyncio
async def test_turn_checkpoint_reports_status(api_client):
    """Turn checkpoint note should report what the agent has done so far."""
    messages = [
        {"role": "user", "content": (
            "Refactor the authentication module. Fix src/auth/login.py, "
            "src/auth/session.py, and src/auth/middleware.py."
        )},
        {"role": "assistant", "content": (
            "I've started reading the auth module. src/auth/login.py has been "
            "fixed — updated the deprecated hashlib call to use the new API. "
            "Now moving on to src/auth/session.py."
        )},
    ]

    result = await run_checkpoint_note(
        task_id="fix-auth",
        agent_run_id="run-fix-auth",
        messages=messages,
        prompt=TURN_CHECKPOINT_PROMPT,
        trigger="turn",
        api_client=api_client,
    )

    assert result.task_id == "fix-auth"
    assert result.trigger == "turn"
    assert len(result.note_summary) > 20, f"Note too short: {result.note_summary}"


# ---------------------------------------------------------------------------
# Test 3: Stuck agent — note should reflect blocked status
# ---------------------------------------------------------------------------


@pytest.mark.skipif(not HAS_CREDENTIALS, reason="No credentials")
@pytest.mark.asyncio
async def test_stuck_agent_checkpoint_reflects_blocked(api_client):
    """Agent that hit an error should produce a note reflecting the blocked state."""
    messages = [
        {"role": "user", "content": (
            "Fix the serializer validation in pkg/serializer.py."
        )},
        {"role": "assistant", "content": (
            "I've read pkg/serializer.py. It imports validate_payload from pkg/schema.py. "
            "When I tried to run tests, I got: "
            "ValidationError: field 'created_at' expects datetime, got str. "
            "This error comes from pkg/schema.py which was changed by another task — "
            "DateTimeField now requires strict datetime objects instead of auto-coercing strings. "
            "I cannot fix this from pkg/serializer.py alone because the schema change "
            "affects all callers."
        )},
    ]

    result = await run_checkpoint_note(
        task_id="fix-serializer",
        agent_run_id="run-fix-serializer",
        messages=messages,
        prompt=TURN_CHECKPOINT_PROMPT,
        trigger="turn",
        api_client=api_client,
    )

    assert len(result.note_summary) > 20
    note_lower = result.note_summary.lower()
    # Should mention the error or blocked state
    has_blocked_signal = any(
        kw in note_lower
        for kw in ("error", "block", "fail", "cannot", "stuck", "schema", "validation")
    )
    assert has_blocked_signal, (
        f"Stuck agent note should mention the error/blocked state: {result.note_summary}"
    )


# ---------------------------------------------------------------------------
# Test 4: Empty snapshot — still produces a valid note
# ---------------------------------------------------------------------------


@pytest.mark.skipif(not HAS_CREDENTIALS, reason="No credentials")
@pytest.mark.asyncio
async def test_empty_snapshot_produces_note(api_client):
    """Even with no prior conversation, checkpoint should produce a valid note."""
    result = await run_checkpoint_note(
        task_id="new-task",
        agent_run_id="run-new-task",
        messages=[],
        prompt=TURN_CHECKPOINT_PROMPT,
        trigger="turn",
        api_client=api_client,
    )

    assert len(result.note_summary) > 0, "Should produce some note even with empty snapshot"
    assert result.turns_used >= 1


# ---------------------------------------------------------------------------
# Test 5: Edit vs turn prompts produce different focus
# ---------------------------------------------------------------------------


@pytest.mark.skipif(not HAS_CREDENTIALS, reason="No credentials")
@pytest.mark.asyncio
async def test_edit_and_turn_prompts_differ(api_client):
    """Edit checkpoint focuses on file changes; turn checkpoint focuses on status."""
    messages = [
        {"role": "user", "content": "Fix pkg/parser.py — update deprecated API calls."},
        {"role": "assistant", "content": (
            "I've edited pkg/parser.py: replaced `parse_raw()` with `model_validate_json()` "
            "on lines 15, 28, and 42. Also updated pkg/parser_utils.py line 7. "
            "Tests are passing. Working on the remaining util functions."
        )},
    ]

    edit_result = await run_checkpoint_note(
        task_id="fix-parser",
        agent_run_id="run-fix-parser-edit",
        messages=messages,
        prompt=EDIT_CHECKPOINT_PROMPT,
        trigger="edit",
        api_client=api_client,
    )

    turn_result = await run_checkpoint_note(
        task_id="fix-parser",
        agent_run_id="run-fix-parser-turn",
        messages=messages,
        prompt=TURN_CHECKPOINT_PROMPT,
        trigger="turn",
        api_client=api_client,
    )

    # Both should produce meaningful content
    assert len(edit_result.note_summary) > 20
    assert len(turn_result.note_summary) > 20
    # They should not be identical (different prompt focus)
    assert edit_result.note_summary != turn_result.note_summary, (
        "Edit and turn notes should differ in focus"
    )
