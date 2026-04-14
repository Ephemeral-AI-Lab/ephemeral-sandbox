"""Task-center note — spawns an ephemeral agent to generate a progress note."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any

from external_trigger.runner import run
from tools.context.toolkit import PostNoteTool, PostNoteInput


TC_NOTE_EDIT_PROMPT = (
    "Write a progress note for the Task Center about this agent's edits.\n"
    "Focus on: what files were edited and why.\n"
    "Call post_note with:\n"
    "- content: name specific files, errors, and changes (under 300 words)\n"
    "- paths: list every file/dir path edited or investigated\n"
    "- tags: one or more of implementation, bug_fix, refactor, blocker, warning "
    "(use 'blocker' if stuck)"
)

TC_NOTE_TURN_PROMPT = (
    "Write a progress note for the Task Center about this agent's work.\n"
    "Include: accomplishments, status (working/stuck/done), and whether "
    "blocked by another task's changes (name the file and error).\n"
    "Call post_note with:\n"
    "- content: name specific files, errors, and scope paths (under 300 words)\n"
    "- paths: list every file/dir path relevant to the work\n"
    "- tags: one or more of implementation, bug_fix, blocker, warning, discovery "
    "(use 'blocker' if stuck or blocked by another task)"
)

TC_NOTE_SYSTEM_PROMPT = (
    "You are a progress reporter. Read the agent's conversation and "
    "produce a concise progress note. Report facts only — do not "
    "instruct the agent or suggest next steps."
)


@dataclass
class NoteSummary:
    """Result of a tc_note generation."""

    task_id: str
    trigger: str  # "edit" or "turn"
    content: str
    turns_used: int = 0
    tags: list[str] | None = None
    paths: list[str] | None = None


async def run_tc_note(
    *,
    task_id: str,
    agent_run_id: str,
    messages: list[dict[str, Any]],
    prompt: str,
    trigger: str,
    max_tokens: int = 500,
    model: str | None = None,
    api_client: Any,
) -> NoteSummary:
    """Spawn an ephemeral agent to generate a task-center progress note.

    The agent inherits the task's conversation snapshot and has only the
    post_note tool available. Uses runner.run() for guaranteed tool call.
    """
    result = await run(
        agent_name=f"tc_note:{task_id}",
        messages=messages,
        system_prompt=TC_NOTE_SYSTEM_PROMPT,
        prompt=prompt,
        tools=[PostNoteTool()],
        api_client=api_client,
        max_tokens_per_turn=max_tokens,
        model=model,
    )

    validated = result.validated
    if not isinstance(validated, PostNoteInput):
        raise RuntimeError(
            f"run_tc_note (task={task_id}): runner returned unexpected "
            f"validated type {type(validated).__name__}, expected PostNoteInput"
        )

    return NoteSummary(
        task_id=task_id,
        trigger=trigger,
        content=validated.content,
        turns_used=result.turns_used,
        tags=validated.tags,
        paths=validated.paths,
    )
