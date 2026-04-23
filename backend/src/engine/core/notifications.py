"""Builders for system notifications emitted by the engine query loop.

Each helper returns ``None`` when no notification should fire, or a
``(history_message, stream_event)`` pair the loop appends to
``display_messages`` and yields to subscribers respectively. Keeping
this logic out of :mod:`engine.core.query` makes the loop body about
control flow only and gives notifications a single, easy-to-test home.
"""

from __future__ import annotations

import math
from typing import TYPE_CHECKING

from message.messages import ConversationMessage
from message.stream_events import SystemNotification

if TYPE_CHECKING:
    from engine.core.query import QueryContext


def _budget_warning_steps(context: "QueryContext") -> str:
    role = ""
    if context.tool_metadata is not None:
        role = str(context.tool_metadata.get("role", "") or "")

    if role == "planner":
        return (
            "1. Stop exploring and shaping new lanes immediately.\n"
            "2. Call submit_plan() with the strongest plan you can defend right now. If the budget is nearly gone, submit_plan() is your next call."
        )
    if role == "replanner":
        return (
            "1. Stop reopening ownership questions immediately.\n"
            "2. Call submit_replan() with the corrective action you can already justify. If the budget is nearly gone, submit_replan() is your next call."
        )
    if role == "reviewer":
        return (
            "1. Reserve one call for submit_task_success or request_replan; never spend the last tool call on daytona_shell, reads, diagnostics, or cleanup.\n"
            "2. Run one final exact verification command (daytona_shell) only if you can still reserve the terminal submission call.\n"
            "3. Call submit_task_success() for PASS with exact commands, exit codes, and diagnostics status, or request_replan() with exact evidence for FAILURE."
        )
    return (
        "1. Reserve one call for submit_task_success or request_replan; never spend the last tool call on daytona_shell, reads, diagnostics, or cleanup.\n"
        "2. Use only evidence already gathered before this warning; do not run one more verification, diagnostic, read, or edit.\n"
            "3. If evidence is incomplete, diagnostics-only, verification was not already green, verification still fails, or diagnostics are absent, call request_replan() with the exact evidence now; this includes collection, import, pytest-config, or environment failures even if they look unrelated.\n"
        "4. If the latest required verification was already green after the final edit and diagnostics were already clean, call submit_task_success() with behavior/API delta, exact commands and exit codes, and diagnostics status."
    )


def build_budget_warning(
    context: "QueryContext",
) -> tuple[ConversationMessage, SystemNotification] | None:
    """Warn the agent that its ``tool_call_limit`` is nearly exhausted.

    Fires when:
      - ``tool_call_limit`` is set, AND
      - used budget has reached 75% of the limit OR only 1 call remains, AND
      - the budget is not yet fully exhausted (``remaining > 0``).

    Returns ``(message, event)``: the loop appends ``message`` to
    ``display_messages`` so the agent's next turn sees it, then yields
    ``event`` so subscribers (eval harness, UI) get a structured notice.
    """
    limit = context.tool_call_limit
    if limit is None:
        return None
    remaining = limit - context.tool_calls_used
    if remaining <= 0:
        return None  # exhausted — handled by loop termination
    used_threshold = max(1, math.ceil(limit * 0.75))
    should_warn = context.tool_calls_used in {used_threshold, limit - 1}
    if not should_warn:
        return None
    if context.last_budget_warning_remaining == remaining:
        return None
    context.last_budget_warning_remaining = remaining
    text = (
        f"[budget warning] Only {remaining} of {limit} tool calls remain "
        f"({context.tool_calls_used} already used). "
        f"Stop editing and exploring immediately. Terminal submission counts against this budget; "
        f"keep one call reserved for the role-correct terminal tool. "
        f"Prepare to enter the terminal summarization flow soon. Your next actions must be:\n"
        f"{_budget_warning_steps(context)}\n"
        f"Do NOT start new edits, file reads, probes, diagnostics, alternate tests, or debugging loops. "
        f"A known next fix is not an exception; preserve it in request_replan(). "
        f"Any non-terminal mutation or investigation after this warning is a contract violation. Submit now."
    )
    return (
        ConversationMessage.from_user_text(text),
        SystemNotification(text=text, category="budget_warning"),
    )
