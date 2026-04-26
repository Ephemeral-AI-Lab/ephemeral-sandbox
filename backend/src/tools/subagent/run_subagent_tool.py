"""run_subagent — spawn a focused worker subagent as a background task.

Backgrounded by the engine (``background="always"``) so the parent can keep
working while the subagent runs. Peek progress with
``check_background_task_result(task_id)``; block on completion with
``wait_background_tasks()``.

The subagent must terminate via a registered terminal tool (typically
``submit_exploration_result``); whatever ``ToolResult`` the engine stamps
with ``does_terminate=True`` becomes this tool's output. If the subagent
exits without calling a terminal tool, the bg task is marked failed and
``check_background_task_result`` falls back to the message peek.

Subagents cannot spawn further subagents — recursion is rejected at
validation time so the focused-worker contract holds.
"""

from __future__ import annotations

import json
import logging
from dataclasses import dataclass
from typing import Any

from pydantic import BaseModel, Field

from message.messages import (
    ConversationMessage,
    TextBlock,
    ThinkingBlock,
    ToolResultBlock,
    ToolUseBlock,
)
from tools.core.base import ExecutionMetadata, TextToolOutput, ToolExecutionContext, ToolResult
from tools.core.decorator import tool

logger = logging.getLogger(__name__)


# Hard upper bound on the peek window — even if a caller requests more,
# the subagent peek clamps to this so the parent's peek response stays bounded.
PEEK_MESSAGE_MAX = 10
_PEEK_BLOCK_CHAR_CAP = 200
_PEEK_TOTAL_CHAR_CAP = 2048


@dataclass
class _ValidatedRunSubagentRequest:
    sub_def: Any


def _truncate(s: str) -> str:
    s = s.replace("\n", " ").strip()
    if len(s) > _PEEK_BLOCK_CHAR_CAP:
        return s[: _PEEK_BLOCK_CHAR_CAP - 1] + "…"
    return s


def _compact_args(inp: Any) -> str:
    try:
        s = json.dumps(inp, separators=(",", ":"), default=str)
    except Exception:
        s = str(inp)
    return _truncate(s)


def _render_block(block: Any) -> str:
    if isinstance(block, TextBlock):
        return f"[text] {_truncate(block.text)}"
    if isinstance(block, ThinkingBlock):
        return f"[think] {_truncate(block.text)}"
    if isinstance(block, ToolUseBlock):
        return f"[tool] {block.name}({_compact_args(block.input)})"
    if isinstance(block, ToolResultBlock):
        return f"[result] {_truncate(str(block.content))}"
    return ""


def format_last_n_messages(messages: list[ConversationMessage], n: int) -> str:
    """Render the last *n* messages of a subagent for the parent's peek view."""
    if not messages:
        return "(no messages yet)"
    n = min(n, PEEK_MESSAGE_MAX)
    tail = messages[-n:]
    rendered: list[str] = []
    for msg in tail:
        prefix = "U:" if msg.role == "user" else "A:"
        for block in msg.content:
            line = _render_block(block)
            if line:
                rendered.append(f"{prefix} {line}")
    if not rendered:
        return "(no renderable content yet)"
    out = "\n".join(rendered)
    if len(out) > _PEEK_TOTAL_CHAR_CAP:
        out = "…" + out[-(_PEEK_TOTAL_CHAR_CAP - 1):]
    return out


class RunSubagentInput(BaseModel):
    """Runtime input model for run_subagent."""

    agent_name: str = Field(
        ...,
        description="Name of a registered dispatchable subagent.",
    )
    prompt: str = Field(
        ...,
        min_length=1,
        description=(
            "Free-form, fully descriptive task prompt. Include any target "
            "paths, context, and required actions inline — this is the only "
            "channel the subagent receives."
        ),
    )


def _validate_run_subagent_request(
    *,
    agent_name: str,
    prompt: str | None,
    context: ToolExecutionContext,
) -> ToolResult | _ValidatedRunSubagentRequest:
    from agents import get_definition

    parent_cfg = context.metadata.runtime_config
    if parent_cfg is None:
        return ToolResult(
            output="run_subagent: missing runtime_config in execution context",
            is_error=True,
        )

    if not isinstance(prompt, str) or not prompt.strip():
        return ToolResult(
            output="run_subagent: `prompt` must be a non-empty string.",
            is_error=True,
        )

    caller_agent_type = context.metadata.get("agent_type")
    if caller_agent_type == "subagent":
        return ToolResult(
            output=(
                "run_subagent: subagents may not spawn further subagents. "
                "This is a hard contract — handle the work directly or "
                "submit your findings via the terminal tool."
            ),
            is_error=True,
        )

    sub_def = get_definition(agent_name)
    if sub_def is None:
        return ToolResult(
            output=f"run_subagent: agent '{agent_name}' is not registered.",
            is_error=True,
        )
    if sub_def.agent_type != "subagent":
        return ToolResult(
            output=(
                f"run_subagent: agent '{agent_name}' is not a subagent "
                f"(agent_type={sub_def.agent_type!r}); "
                "only subagent-typed agents may be dispatched here."
            ),
            is_error=True,
        )
    return _ValidatedRunSubagentRequest(sub_def=sub_def)


@tool(
    name="run_subagent",
    description=(
        "Spawns a registered subagent as a background task. The subagent "
        "receives `prompt` as its only input and must finish by calling its "
        "terminal tool (typically submit_exploration_result); that tool's "
        "text output is delivered as this tool's result. Use "
        "check_background_task_result(task_id) to peek progress or fetch "
        "the finished result."
    ),
    short_description="Spawn a subagent in the background.",
    input_model=RunSubagentInput,
    output_model=TextToolOutput,
    background="always",
    task_type="subagent",
)
async def run_subagent(
    agent_name: str,
    prompt: str,
    *,
    context: ToolExecutionContext,
) -> ToolResult:
    """Spawn a named subagent and rejoin via the background-task lifecycle."""
    from engine.runtime.lifecycle import run_ephemeral_agent

    validation = _validate_run_subagent_request(
        agent_name=agent_name,
        prompt=prompt,
        context=context,
    )
    if isinstance(validation, ToolResult):
        return validation
    sub_def = validation.sub_def

    parent_cfg = context.metadata.runtime_config
    sandbox_id = context.metadata.sandbox_id or None
    bg_manager = context.metadata.background_task_manager
    bg_task_id = context.metadata.background_task_id

    sub_meta = ExecutionMetadata()
    sub_meta["agent_type"] = "subagent"
    if sub_def.role:
        sub_meta["role"] = sub_def.role

    def _on_spawned(agent: Any) -> None:
        # Register the live-peek provider so check_background_task_result
        # can render the inner agent's last N messages while it's running
        # (and after, if the terminal tool was never called).
        if bg_manager is None or not isinstance(bg_task_id, str):
            return
        bg_manager.set_progress_provider(
            bg_task_id,
            lambda last_n: format_last_n_messages(agent.display_messages, last_n),
        )

    result = await run_ephemeral_agent(
        parent_cfg,
        prompt,
        agent_def=sub_def,
        sandbox_id=sandbox_id,
        persist_agent_run=False,
        extra_tool_metadata=sub_meta,
        on_agent_spawned=_on_spawned,
    )

    # Stamp the metadata flag check_background_task_result uses to
    # distinguish "finished with terminal result" from "finished without
    # calling the terminal tool" (which we want to report as failed).
    if result.status == "failed":
        return ToolResult(
            output=f"run_subagent: subagent crashed: {result.error}",
            is_error=True,
            metadata={"subagent_terminal_called": False},
        )
    if result.terminal_result is None:
        return ToolResult(
            output=(
                "run_subagent: subagent exited without calling a terminal tool. "
                "The findings were not delivered."
            ),
            is_error=True,
            metadata={"subagent_terminal_called": False},
        )
    terminal = result.terminal_result
    return ToolResult(
        output=terminal.output,
        is_error=terminal.is_error,
        metadata={**(terminal.metadata or {}), "subagent_terminal_called": True},
    )
