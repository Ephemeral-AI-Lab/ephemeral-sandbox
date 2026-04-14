"""Pause assessment — spawns an ephemeral agent to assess blocker impact."""

from __future__ import annotations

import logging
from dataclasses import dataclass, field
from typing import Any

from external_trigger.agent import run_external_trigger
from tools.external_trigger.pause_verdict import PauseVerdictInput, PauseVerdictTool

logger = logging.getLogger(__name__)


@dataclass
class PauseVerdict:
    """Result of a blocker impact assessment."""

    task_id: str = ""
    answer: str = ""  # "YES", "NO"
    reason: str = ""
    conversation: list[dict] = field(default_factory=list)
    turns_used: int = 0


async def assess_pause(
    *,
    task_id: str,
    agent_run_id: str,
    messages: list[dict],
    system_prompt: str,
    broken_files: list[str],
    problem: str,
    api_client: Any,
    model: str | None = None,
) -> PauseVerdict:
    """Spawn an ephemeral agent to assess whether a running task is affected by a blocker.

    The agent inherits the task's conversation snapshot and has only the
    pause_verdict tool available. Uses runner.run() for guaranteed tool call.
    """
    prompt = (
        "BLOCKER CHECK\n"
        "A shared dependency has been reported broken.\n"
        f"Broken files: {', '.join(broken_files)}\n"
        f"Problem: {problem}\n"
        "\n"
        "Based on your work so far in this conversation,\n"
        "does your task depend on any of these files?\n"
        "Call the pause_verdict tool with your assessment."
    )

    result = await run_external_trigger(
        agent_name=f"pause_assessor:{task_id}",
        messages=messages,
        system_prompt=system_prompt,
        prompt=prompt,
        tools=[PauseVerdictTool()],
        api_client=api_client,
        max_tokens_per_turn=200,
        model=model,
    )

    validated = result.validated
    if isinstance(validated, PauseVerdictInput):
        return PauseVerdict(
            task_id=task_id,
            answer=validated.answer,
            reason=validated.reason,
            conversation=result.conversation,
            turns_used=result.turns_used,
        )

    # Should not reach here — runner guarantees validated output.
    return PauseVerdict(
        task_id=task_id,
        answer="NO",
        reason="unexpected validation state",
        conversation=result.conversation,
    )
