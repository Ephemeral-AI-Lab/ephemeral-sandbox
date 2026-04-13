"""EphemeralTask module — single-shot LLM calls for Conductor and TaskCenter active mode."""

from ephemeral_task.core import EphemeralTaskResult, Snapshot, call_llm, call_llm_tool
from ephemeral_task.pause_assessment import PAUSE_VERDICT_TOOL, PauseVerdict, assess_pause
from ephemeral_task.tc_note import (
    CHECKPOINT_SYSTEM_PROMPT,
    EDIT_CHECKPOINT_PROMPT,
    POST_NOTE_TOOL,
    TURN_CHECKPOINT_PROMPT,
    NoteSummary,
    run_ephemeral_task as run_ephemeral_note,
    run_ephemeral_task,
)

__all__ = [
    "CHECKPOINT_SYSTEM_PROMPT",
    "EDIT_CHECKPOINT_PROMPT",
    "EphemeralTaskResult",
    "NoteSummary",
    "PAUSE_VERDICT_TOOL",
    "POST_NOTE_TOOL",
    "PauseVerdict",
    "Snapshot",
    "TURN_CHECKPOINT_PROMPT",
    "assess_pause",
    "call_llm",
    "call_llm_tool",
    "run_ephemeral_note",
    "run_ephemeral_task",
]
