"""``ask_advisor`` — blocking read-only audit before terminal submission.

The advisor sees the parent's contract verbatim (user_msg_1 + user_msg_2),
a filtered transcript of what the parent did, and its own audit framing
(terminal-tool catalog with advisor_review_focus + the pending submission).
It returns ``approve`` or ``reject`` via ``submit_advisor_feedback``.
"""

from __future__ import annotations

import json

from pydantic import BaseModel, Field

from sandbox._shared.models import Intent
from tools._framework.core.context import ToolExecutionContextService
from tools._framework.core.decorator import tool
from tools._framework.core.results import TextToolOutput, ToolResult
from tools._hooks.block_in_isolated_mode import BlockInIsolatedMode
from tools._terminals.registry import render_terminal_catalog
from .prompt import get_ask_advisor_description
from tools.ask_helper._lib._compose import (
    HelperMessageError,
    HelperMessages,
    as_initial_message,
    assemble_user_msg_1,
    build_helper_messages,
)


class AskAdvisorInput(BaseModel):
    tool_name: str = Field(
        ...,
        min_length=1,
        description=(
            "The name of the terminal tool you intend to call (e.g. submit_generator_outcome)."
        ),
    )
    tool_payload: dict[str, object] = Field(
        default_factory=dict,
        description=(
            "The arguments you intend to pass to the terminal tool. The "
            "advisor reviews payload quality against the contract."
        ),
    )


def _render_pending_submission(*, tool_name: str, tool_payload: dict[str, object]) -> str:
    payload_json = json.dumps(tool_payload, indent=2, sort_keys=True, default=str)
    return (
        "# Pending submission\n\n"
        "The parent intends to call:\n\n"
        f"Tool: `{tool_name}`\n\n"
        f"Arguments:\n```json\n{payload_json}\n```"
    )


def _render_catalog_section(messages: HelperMessages) -> str:
    """Render the advisor's `# Terminal tool catalog (advisor review focus)`.

    Falls back to a stub line when the parent's terminals are unknown
    (parent agent_name missing or unregistered). The advisor still runs;
    selection auditability is the only thing degraded.
    """
    terminals = list(messages.parent_active_terminals)
    parent_def = messages.parent_agent_def
    if not terminals and parent_def is not None:
        terminals = list(parent_def.terminals)
    if not terminals:
        return (
            "# Terminal tool catalog (advisor review focus)\n\n"
            "(parent terminals unavailable — review the pending submission "
            "against the parent's original task as best you can)"
        )
    catalog = render_terminal_catalog(terminals, focus="advisor_review_focus")
    return (
        "# Terminal tool catalog (advisor review focus)\n\n"
        "The parent could submit any of the following terminals. Review "
        "focus for each:\n\n"
        f"{catalog}\n\n"
        "These entries pair with the parent-facing selection criteria the "
        "parent saw in its original task; both views come from the same "
        "terminal-tool registry."
    )


_ADVISOR_TASK_SECTION = (
    "# Your task\n\n"
    "Review two distinct things:\n\n"
    "1. **Tool selection** — using the parent's original context, original "
    "task, and transcript as evidence, did the parent pick the right "
    "terminal from the catalog above? Or should it have called a different "
    "terminal?\n\n"
    "2. **Quality of synthesis/exploration backing the payload** — does the "
    "transcript actually support the payload's claims? Flag stubs, TODOs, "
    "unverified assertions, missed acceptance criteria, or claims that "
    "exceed what the transcript shows.\n\n"
    "Quote transcript lines or contract fragments to ground your findings. "
    "Falsifiable beats vague."
)

_ADVISOR_CALIBRATION_SECTION = (
    "# Calibration\n\n"
    "Apply a lenient approve bar:\n\n"
    "- approve when the tool choice is right and the payload is plausibly "
    "supported by the transcript, even if the work isn't pristine.\n\n"
    "- reject only on real quality problems: wrong terminal selection, or "
    "synthesis/exploration that doesn't support the payload's claims (stubs, "
    "TODOs, deliverable missing or misnamed, criteria not actually "
    "exercised).\n\n"
    'If the parent has already received a prior "reject" in this run '
    "(visible in the transcript as a prior ask_advisor call), check whether "
    "the parent addressed the prior issues. A parent that ignored prior "
    "feedback warrants a sharper second reject."
)

_ADVISOR_HOW_TO_SUBMIT_SECTION = (
    "# How to submit\n\n"
    "Call `submit_advisor_feedback` exactly once with:\n\n"
    '- `verdict`: "approve" or "reject".\n\n'
    "- `summary`: focused prose that MUST cover, in order:\n\n"
    '  1. Tool selection — "correct" or "should be <other_tool>" with a '
    "one-sentence rationale.\n\n"
    "  2. Quality of synthesis/exploration backing the payload — what's "
    "solid, what's thin or unsupported. Quote transcript lines or contract "
    "fragments.\n\n"
    "  3. Residual risks (if any) — issues the parent should weigh even on "
    "approve, or the single most important thing to fix before re-attempting "
    'on reject. "None" if none.\n\n'
    "Be concise. Falsifiable beats vague. No filler."
)


def _build_advisor_user_msg_2(
    *,
    messages: HelperMessages,
    tool_name: str,
    tool_payload: dict[str, object],
) -> str:
    return "\n\n".join(
        [
            _render_catalog_section(messages),
            _render_pending_submission(tool_name=tool_name, tool_payload=tool_payload),
            _ADVISOR_TASK_SECTION,
            _ADVISOR_CALIBRATION_SECTION,
            _ADVISOR_HOW_TO_SUBMIT_SECTION,
        ]
    )


@tool(
    name="ask_advisor",
    description=get_ask_advisor_description(),
    input_model=AskAdvisorInput,
    output_model=TextToolOutput,
    intent=Intent.READ_ONLY,
    pre_hooks=(BlockInIsolatedMode("ask_advisor"),),
)
async def ask_advisor(
    tool_name: str,
    tool_payload: dict[str, object],
    *,
    context: ToolExecutionContextService,
) -> ToolResult:
    from engine.api import run_ephemeral_agent

    runtime_config = context.runtime_config
    if runtime_config is None:
        return ToolResult(
            output="ask_advisor: missing runtime_config in execution context.",
            is_error=True,
        )

    try:
        messages = build_helper_messages(helper_role="advisor", context=context)
    except HelperMessageError as exc:
        return exc.to_tool_result()

    user_msg_1 = assemble_user_msg_1(messages)
    user_msg_2 = _build_advisor_user_msg_2(
        messages=messages, tool_name=tool_name, tool_payload=tool_payload
    )

    result = await run_ephemeral_agent(
        runtime_config,
        user_msg_2,
        agent_def=messages.helper_agent_def,
        sandbox_id=context.sandbox_id or None,
        persist_agent_run=False,
        extra_tool_metadata=context.services_with_overrides(
            role="advisor",
            agent_type="agent",
        ),
        initial_messages=[as_initial_message(user_msg_1)],
    )
    if result.status == "failed":
        return ToolResult(
            output=f"ask_advisor: advisor crashed: {result.error}",
            is_error=True,
        )
    if result.terminal_result is None:
        return ToolResult(
            output="ask_advisor: advisor exited without submit_advisor_feedback.",
            is_error=True,
        )
    terminal = result.terminal_result
    return ToolResult(
        output=terminal.output,
        is_error=terminal.is_error,
        metadata=dict(terminal.metadata or {}),
    )
