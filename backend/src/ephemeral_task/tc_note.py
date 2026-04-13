"""CheckpointTask — progress note generation via forced tool call."""

from __future__ import annotations

import time
from dataclasses import dataclass

from ephemeral_task.core import EphemeralTaskResult, Snapshot

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

POST_NOTE_TOOL = {
    "name": "post_note",
    "description": "Post a progress note to the Task Center summarizing this agent's work.",
    "input_schema": {
        "type": "object",
        "properties": {
            "content": {
                "type": "string",
                "description": "Note content to post. Include file paths, changes made, and current status.",
            },
            "scope_paths": {
                "type": "array",
                "items": {"type": "string"},
                "description": "File/dir scope for filtering. Defaults to the task's write_scope.",
            },
        },
        "required": ["content"],
    },
}


@dataclass
class NoteSummary(EphemeralTaskResult):
    """Parsed checkpoint note result — extends EphemeralTaskResult."""

    task_id: str = ""
    trigger: str = ""  # "edit" or "turn"
    note_summary: str = ""
    status: str = ""
    blocked_by: str = ""
    elapsed_seconds: float = 0.0


async def run_ephemeral_task(
    *,
    snapshot: Snapshot,
    prompt: str,
    trigger: str = "",
    max_tokens: int = 500,
    model: str | None = None,
    api_client: object,
    timeout_seconds: float | None = None,
) -> NoteSummary:
    """Single-shot checkpoint call with forced tool use. Returns NoteSummary."""
    started = time.monotonic()
    result = await snapshot.ask_tool(
        prompt,
        tool=POST_NOTE_TOOL,
        api_client=api_client,
        max_tokens=max_tokens,
        model=model,
        timeout_seconds=timeout_seconds,
    )

    note_text = result.text
    note_summary = ""

    if result.tool_input is not None:
        note_summary = str(
            result.tool_input.get("content")
            or result.tool_input.get("note")
            or ""
        )
        note_text = note_summary

    return NoteSummary(
        text=note_text,
        timed_out=result.timed_out,
        task_id=snapshot.task_id,
        trigger=trigger,
        note_summary=note_summary,
        status=str((result.tool_input or {}).get("status", "")),
        blocked_by=str((result.tool_input or {}).get("blocked_by", "")),
        elapsed_seconds=max(0.0, time.monotonic() - started),
    )
