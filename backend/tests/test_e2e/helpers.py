"""Shared helpers for E2E background-task tests.

Centralises the ``_log_result`` and ``_assert_fg_during_bg`` functions that
were previously copy-pasted across every ``test_bg_*.py`` file.
"""

from __future__ import annotations

import logging

logger = logging.getLogger(__name__)


# ---------------------------------------------------------------------------
# Result logging
# ---------------------------------------------------------------------------


def log_result(result, label: str, *, extra_fields: dict[str, int] | None = None) -> None:
    """Log an EvalResult summary for debugging.

    Parameters
    ----------
    result:
        The ``EvalResult`` returned by ``agent.invoke()``.
    label:
        Human-readable tag shown in the log line (e.g. ``"triple_bg"``).
    extra_fields:
        Optional mapping of ``{display_name: value}`` appended after the
        standard counters.  Callers that previously logged scenario-specific
        metrics (e.g. ``"File writes"``) can pass them here.
    """
    bg_started = result.background_started()
    bg_completed = result.background_completed()
    checks = result.tool_count("check_background_progress")
    waits = result.tool_count("wait_for_background_task")
    cancels = result.tool_count("cancel_background_task")

    lines = [
        f"\n{'=' * 60}",
        f"[{label}] Event summary:",
        f"  Tools started: {len(result.tools_started())}",
        f"  Tools completed: {len(result.tools_completed())}",
        f"  Background started: {len(bg_started)}",
        f"  Background completed: {len(bg_completed)}",
        f"  Progress checks: {checks}",
        f"  Wait calls: {waits}",
        f"  Cancels: {cancels}",
    ]
    if extra_fields:
        for name, value in extra_fields.items():
            lines.append(f"  {name}: {value}")
    lines.append(f"  Tool sequence: {result.tool_names}")
    lines.append(f"{'=' * 60}")

    logger.info("\n".join(lines))


# ---------------------------------------------------------------------------
# Concurrency assertion
# ---------------------------------------------------------------------------


def assert_fg_during_bg(result, min_fg: int = 1) -> None:
    """Assert that foreground tool calls happened WHILE background tasks were running.

    Verifies that at least *min_fg* foreground ``daytona_shell`` / ``daytona_write_file``
    calls occurred between the first background launch and the first
    ``check_background_progress`` or ``cancel_background_task`` call.
    This proves true fg+bg concurrency.
    """
    bg_start_indices = [
        i
        for i, tc in enumerate(result.tool_calls)
        if tc.name == "daytona_shell" and tc.input.get("background") is True
    ]
    lifecycle_indices = [
        i
        for i, tc in enumerate(result.tool_calls)
        if tc.name in ("check_background_progress", "cancel_background_task")
    ]
    assert bg_start_indices, "No background launches found"
    assert lifecycle_indices, "No check/cancel calls found"

    first_bg = bg_start_indices[0]
    first_lifecycle = lifecycle_indices[0]

    fg_during_bg = [
        tc
        for i, tc in enumerate(result.tool_calls)
        if first_bg < i < first_lifecycle
        and tc.name in ("daytona_shell", "daytona_write_file")
        and not tc.input.get("background")
    ]
    assert len(fg_during_bg) >= min_fg, (
        f"Expected {min_fg}+ foreground calls BETWEEN bg launch (idx {first_bg}) "
        f"and first check/cancel (idx {first_lifecycle}). "
        f"Got {len(fg_during_bg)} fg calls in that window. "
        f"Full sequence: {result.tool_names}"
    )
