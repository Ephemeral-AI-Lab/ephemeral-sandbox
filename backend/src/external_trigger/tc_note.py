"""Checkpoint note — spawns an ephemeral agent to generate a progress note."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any

from external_trigger.agent import run_external_trigger
from tools.context.toolkit import PostNoteTool, PostNoteInput


EDIT_CHECKPOINT_PROMPT = (
    "Based on this agent's work so far, write a progress note "
    "for the Task Center.\n"
    "Focus on: what files were edited and why.\n"
    "Include file paths and specific changes made.\n"
    "Keep under 300 words.\n"
    "Call the post_note tool with your note."
)

TURN_CHECKPOINT_PROMPT = (
    "Based on this agent's work so far, write a progress note "
    "for the Task Center.\n"
    "Include:\n"
    "1. What the agent has accomplished\n"
    "2. Current status (working / stuck / nearly done)\n"
    "3. Whether the agent appears blocked by code that another "
    "task broke (include the file path and error if so)\n"
    "Keep under 300 words.\n"
    "Call the post_note tool with your note."
)

CHECKPOINT_SYSTEM_PROMPT = (
    "You are a progress reporter. Read the agent's conversation and "
    "produce a concise progress note. Report facts only — do not "
    "instruct the agent or suggest next steps."
)


@dataclass
class NoteSummary:
    """Result of a checkpoint note generation."""

    task_id: str = ""
    trigger: str = ""  # "edit" or "turn"
    note_summary: str = ""
    turns_used: int = 0


async def run_checkpoint_note(
    *,
    task_id: str,
    agent_run_id: str,
    messages: list[dict],
    prompt: str,
    trigger: str = "",
    max_tokens: int = 500,
    model: str | None = None,
    api_client: Any,
) -> NoteSummary:
    """Spawn an ephemeral agent to generate a checkpoint note.

    The agent inherits the task's conversation snapshot and has only the
    post_note tool available. Uses runner.run() for guaranteed tool call.
    """
    result = await run_external_trigger(
        agent_name=f"checkpoint:{task_id}",
        messages=messages,
        system_prompt=CHECKPOINT_SYSTEM_PROMPT,
        prompt=prompt,
        tools=[PostNoteTool()],
        api_client=api_client,
        max_tokens_per_turn=max_tokens,
        model=model,
    )

    validated = result.validated
    if isinstance(validated, PostNoteInput):
        return NoteSummary(
            task_id=task_id,
            trigger=trigger,
            note_summary=validated.content,
            turns_used=result.turns_used,
        )

    # Fallback — extract from raw tool_input
    content = str(result.tool_input.get("content", ""))
    return NoteSummary(
        task_id=task_id,
        trigger=trigger,
        note_summary=content,
        turns_used=result.turns_used,
    )
