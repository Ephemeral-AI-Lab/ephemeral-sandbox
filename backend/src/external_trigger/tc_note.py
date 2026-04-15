"""Task-center note — spawns an ephemeral agent to generate a progress note."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any

from agents.registry import get_definition
from external_trigger.runner import run
from tools.context.toolkit import SubmitTaskNoteTool, PostNoteInput


TC_NOTE_EDIT_PROMPT = (
    "Write a progress note for the Task Center about this agent's edits.\n"
    "Focus on: what files were edited and why.\n"
    "Call submit_task_note with:\n"
    "- content: name specific files, errors, and changes (under 300 words)\n"
    "- paths: list every file/dir path edited or investigated\n"
    "- tags: one or more of implementation, bug_fix, refactor, blocker, warning "
    "(use 'blocker' if stuck)"
)

TC_NOTE_TURN_PROMPT = (
    "Call submit_task_note now. The 'content' field is REQUIRED.\n"
    "- content: what this agent accomplished and current status "
    "(working/stuck/done). Name specific files and errors. Under 300 words.\n"
    "- paths: list every file/dir path relevant to the work\n"
    "- tags: one or more of implementation, bug_fix, blocker, warning, discovery "
    "(use 'blocker' if stuck or blocked by another task)"
)

_DEFAULT_TC_NOTE_SYSTEM_PROMPT = (
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


def _resolve_note_taker_prompt() -> tuple[str, str | None]:
    defn = get_definition("note_taker")
    if defn is None:
        return _DEFAULT_TC_NOTE_SYSTEM_PROMPT, None

    prompt = (defn.system_prompt or "").strip() or _DEFAULT_TC_NOTE_SYSTEM_PROMPT
    model = str(defn.model).strip() if defn.model else ""
    return prompt, (model if model and model != "inherit" else None)


async def run_tc_note(
    *,
    task_id: str,
    agent_run_id: str,
    messages: list[dict[str, Any]],
    prompt: str,
    trigger: str,
    max_tokens: int = 1024,
    model: str | None = None,
    api_client: Any,
) -> NoteSummary:
    """Spawn an ephemeral agent to generate a task-center progress note."""
    system_prompt, default_model = _resolve_note_taker_prompt()
    result = await run(
        agent_name=f"note_taker:{task_id}",
        messages=messages,
        system_prompt=system_prompt,
        prompt=prompt,
        tools=[SubmitTaskNoteTool()],
        api_client=api_client,
        max_tokens_per_turn=max_tokens,
        model=model or default_model,
    )

    validated = result.validated
    if not isinstance(validated, PostNoteInput):
        raise RuntimeError(f"run_tc_note ({task_id}): expected PostNoteInput, got {type(validated).__name__}")

    return NoteSummary(
        task_id=task_id,
        trigger=trigger,
        content=validated.content,
        turns_used=result.turns_used,
        tags=validated.tags,
        paths=validated.paths,
    )
