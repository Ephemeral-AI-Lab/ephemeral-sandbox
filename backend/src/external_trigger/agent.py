"""Ephemeral agent spawner for external_trigger tool calls.

Spawns a lightweight agent identity, inherits a conversation snapshot,
and runs the external_trigger runner with constrained tools.
"""

from __future__ import annotations

import logging
import uuid
from typing import Any

from external_trigger.runner import RunResult, run
from tools.core.base import BaseTool

logger = logging.getLogger(__name__)


async def run_external_trigger(
    *,
    agent_name: str,
    messages: list[dict[str, Any]],
    system_prompt: str,
    prompt: str,
    tools: list[BaseTool],
    api_client: Any,
    max_tokens_per_turn: int = 500,
    model: str | None = None,
) -> RunResult:
    """Spawn an ephemeral agent and run until a valid tool call succeeds.

    The agent:
    - Has a unique identity (agent_name + run_id) for observability
    - Inherits conversation from the snapshot (frozen, read-only)
    - Has only the provided tools available (external_trigger tools)
    - Uses runner.run() for guaranteed tool call (tool_choice="any", retry)

    Parameters
    ----------
    agent_name:
        Identity for logging/observability (e.g. "pause_assessor:task_123").
    messages:
        Frozen conversation snapshot inherited from the assessed task.
    system_prompt:
        System prompt for the ephemeral agent session.
    prompt:
        The question/instruction appended as the final user message.
    tools:
        Constrained tool set — agent must call one of these.
    api_client:
        Anthropic-compatible client with ``create_message()``.
    max_tokens_per_turn:
        Max tokens per LLM response.
    model:
        Model override.
    """
    agent_run_id = str(uuid.uuid4())
    tool_names = [t.name for t in tools]
    logger.info(
        "Spawning external_trigger agent %s (run=%s) with tools=%s",
        agent_name,
        agent_run_id[:8],
        tool_names,
    )

    result = await run(
        messages=messages,
        system_prompt=system_prompt,
        prompt=prompt,
        tools=tools,
        api_client=api_client,
        max_tokens_per_turn=max_tokens_per_turn,
        model=model,
    )

    logger.info(
        "External_trigger agent %s (run=%s) completed: tool=%s turns=%d",
        agent_name,
        agent_run_id[:8],
        result.tool_name,
        result.turns_used,
    )
    return result
