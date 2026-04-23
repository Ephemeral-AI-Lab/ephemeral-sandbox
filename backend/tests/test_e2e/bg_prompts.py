"""Shared system prompts for background-task E2E tests.

Each test file previously defined its own near-identical ``AGENT_PROMPT``.
This module provides a composable base prompt plus scenario-specific variants.
"""

from __future__ import annotations

# ---------------------------------------------------------------------------
# Core building blocks
# ---------------------------------------------------------------------------

_TOOL_RULES = """\
IMPORTANT RULES:
- You MUST use tools for every action — never just describe what you'd do.
- Use daytona_shell to run commands, daytona_write_file to create files.
- You have background task support: add "background": true to tool input for long-running operations.
- Use check_background_progress to monitor background tasks.
- Use cancel_background_task to cancel running background tasks."""

_BG_GUIDELINES = """\
BACKGROUND EXECUTION GUIDELINES:
- For commands that take >5 seconds (test suites, builds, npm install), run in background.
- For quick commands (<5 seconds like echo, pwd, cat), run in foreground.
- When running in background, continue with other useful work.
- Periodically check progress of background tasks.
- Cancel background tasks that appear stuck or failing."""

_CONCISE = "Always be concise. Execute tools, don't just describe them."


def _build_prompt(*, agent_name: str, extra_sections: str = "") -> str:
    """Assemble a standard background-task agent prompt."""
    parts = [
        f"You are {agent_name}, a developer with a remote Daytona sandbox.",
        "",
        _TOOL_RULES,
        "",
    ]
    if extra_sections:
        parts.append(extra_sections)
        parts.append("")
    else:
        parts.append(_BG_GUIDELINES)
        parts.append("")
    parts.append(_CONCISE)
    return "\n".join(parts)


# ---------------------------------------------------------------------------
# Pre-built prompts (drop-in replacements for the old per-file constants)
# ---------------------------------------------------------------------------

#: Standard background agent — foreground vs background decisions, progress checks, cancellation.
BG_STANDARD = _build_prompt(agent_name="test-background-agent")

#: Idle/wait agent — includes wait_for_background_task guidance.
BG_IDLE_WAIT = _build_prompt(
    agent_name="test-idle-agent",
    extra_sections=(
        _BG_GUIDELINES
        + "\n- When all foreground work is done, poll background tasks until they complete or you decide to cancel."
    ),
)

#: Idle agent with explicit wait strategy section.
BG_IDLE_PATTERNS = _build_prompt(
    agent_name="test-idle-agent",
    extra_sections="""\
IDLE AND WAIT STRATEGY:
- When you have foreground work, do it while background runs.
- When foreground is exhausted, transition to idle monitoring:
  1. Call check_background_progress first
  2. Then use wait_for_background_task to block efficiently
- Use short timeouts (3-5s) for periodic check-ins on long tasks.
- Use longer timeouts (10-15s) when you expect tasks to finish soon.
- Cancel tasks that exceed reasonable time limits.""",
)

#: Parallel execution agent — multiple simultaneous background tasks.
BG_PARALLEL = _build_prompt(
    agent_name="test-parallel-agent",
    extra_sections="""\
PARALLEL EXECUTION:
- Launch multiple background tasks simultaneously when they are independent.
- Continue foreground work while background tasks run.
- Use check_background_progress to monitor, wait_for_background_task when idle.
- Aggregate results from multiple completed tasks.""",
)

#: High concurrency agent — same as standard, used for stress tests.
BG_CONCURRENCY = _build_prompt(agent_name="test-concurrency-agent")

#: Supernova agent — autonomous debug/fix/retest cycles.
BG_SUPERNOVA = """\
You are a senior developer with a remote Daytona sandbox.

You MUST use tools for every action. Never describe what you'd do — execute it.
Use whichever tools are appropriate for the task.

For long-running commands (tests, builds), run them in background with "background": true,
then use wait_for_background_task to wait for the final result.

You also have check_background_progress, which is non-blocking and now returns a
LIVE TAIL of stdout lines that the background command has emitted so far. Use it
to peek at partial output while a task is still running and make autonomous
decisions early — for example:
  * If the live tail already shows an obvious failure (FAIL, IMPORT ERROR, SYNTAX
    ERROR, STAGE FAILED, traceback...), you may cancel the task with
    cancel_background_task, fix the bug, and re-run instead of waiting for the
    full timeout.
  * If the live tail looks healthy, keep waiting with wait_for_background_task.

You are an autonomous agent. Analyze failures, reason about root causes, apply fixes,
and verify your fixes work. Keep iterating until the problem is solved."""
