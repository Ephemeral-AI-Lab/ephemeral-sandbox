"""Shared ephemeral-agent lifecycle.

Single entrypoint that spawns an agent, drives its run loop, persists its
audit row, and returns a structured result. Used by both the
top-level chat path (``execute_ephemeral_agent_run``) and the subagent
dispatch tool (``run_subagent``).

The terminal-tool contract is the result-delivery channel: when the agent's
loop exits via a successful ``is_terminal_tool=True`` call, that tool's
``ToolResult`` is exposed on :class:`EphemeralRunResult.terminal_result`. The
parent reads it directly — no envelope, no JSON wrapping, no message
re-extraction.
"""

from __future__ import annotations

import logging
from collections.abc import Awaitable, Callable
from dataclasses import dataclass
from typing import TYPE_CHECKING, Any, Literal

from agents.types import AgentDefinition
from message.messages import ConversationMessage, ToolResultBlock
from message.stream_events import StreamEvent, ToolExecutionCompleted
from tools.core.base import ExecutionMetadata, ToolResult

if TYPE_CHECKING:
    from server.app_factory import RuntimeConfig

logger = logging.getLogger(__name__)

AgentStreamEmitter = Callable[[StreamEvent], Awaitable[None]]

EphemeralRunStatus = Literal["completed", "failed"]


@dataclass
class EphemeralRunResult:
    """Outcome of one :func:`run_ephemeral_agent` invocation."""

    status: EphemeralRunStatus
    error: str | None
    terminal_result: ToolResult | None
    agent_name: str
    event_count: int


def _last_terminal_tool_result(
    messages: list[ConversationMessage],
) -> ToolResult | None:
    """Walk *messages* backwards for the last terminating tool result.

    Identifies the result the engine stamped with ``does_terminate=True`` when
    a ``is_terminal_tool=True`` tool returned non-error. Returns the
    corresponding :class:`ToolResult` (with
    ``output``, ``metadata``, etc.) or ``None`` if the loop exited without a
    terminal call (e.g. resource limit or a plain text response).
    """
    for msg in reversed(messages):
        if msg.role != "user":
            continue
        for block in reversed(msg.content):
            if isinstance(block, ToolResultBlock) and block.does_terminate:
                return ToolResult(
                    output=str(block.content),
                    is_error=block.is_error,
                    metadata=dict(block.metadata or {}),
                    does_terminate=True,
                )
    return None


async def run_ephemeral_agent(
    config: "RuntimeConfig",
    prompt: str,
    *,
    agent_def: AgentDefinition | None = None,
    sandbox_id: str | None = None,
    initial_messages: list[ConversationMessage] | None = None,
    persist_agent_run: bool = True,
    task_id: str | None = None,
    on_event: AgentStreamEmitter | None = None,
    on_agent_spawned: Callable[[Any], None] | None = None,
    extra_tool_metadata: ExecutionMetadata | dict[str, Any] | None = None,
) -> EphemeralRunResult:
    """Spawn → track → run → persist a minimal agent run.

    Single source of truth for the ephemeral-agent lifecycle. TaskCenter
    callers pass ``task_id`` so the run can be attached to the corresponding
    ``task_center_tasks`` row. Subagent dispatches omit ``task_id`` and remain
    transient background work.

    Terminal tools end the run immediately. There is no same-run retry loop;
    callers that need recovery must spawn a fresh agent run with a new prompt.
    """
    from agents.run_tracker import AgentRunTracker
    from engine.runtime.agent import spawn_agent

    db_available = False
    if persist_agent_run and task_id:
        try:
            from server.app_factory import agent_run_store as _ars
            db_available = _ars.is_ready
        except Exception:
            db_available = False

    messages = list(initial_messages or [])

    agent = spawn_agent(
        config,
        messages,
        agent_def=agent_def,
        sandbox_id=sandbox_id,
    )
    if on_agent_spawned is not None:
        try:
            on_agent_spawned(agent)
        except Exception:
            logger.debug("on_agent_spawned hook raised", exc_info=True)
    logger.info(
        "Spawned agent %r (model=%s, task_id=%s)",
        agent.agent_name,
        agent.model,
        task_id,
    )

    tracker = AgentRunTracker.create(
        task_id=task_id if db_available else None,
        agent_name=agent.agent_name,
    )
    agent_run_id = tracker.agent_run_id

    if agent.query_context.tool_metadata is None:
        agent.query_context.tool_metadata = ExecutionMetadata()
    if extra_tool_metadata:
        agent.query_context.tool_metadata.update(extra_tool_metadata)
    if agent_run_id is not None:
        agent.query_context.tool_metadata.agent_run_id = agent_run_id
    agent.query_context.run_id = task_id or agent_run_id or agent.query_context.run_id

    event_count = 0
    run_error: str | None = None
    terminal_result: ToolResult | None = None

    try:
        async for event in agent.run(prompt):
            event_count += 1
            if (
                isinstance(event, ToolExecutionCompleted)
                and event.does_terminate
                and not event.is_error
            ):
                terminal_result = ToolResult(
                    output=event.output,
                    is_error=event.is_error,
                    metadata=dict(event.metadata or {}),
                    does_terminate=True,
                )
            if on_event is not None:
                await on_event(event)
    except Exception as exc:
        run_error = str(exc)
        logger.exception("run_ephemeral_agent: agent run crashed")

    if not run_error and terminal_result is None:
        terminal_result = agent.query_context.terminal_result or _last_terminal_tool_result(
            agent._messages
        )
    if run_error:
        terminal_result = None
    terminal_payload = (
        {
            "output": terminal_result.output,
            "is_error": terminal_result.is_error,
            "metadata": terminal_result.metadata,
            "does_terminate": terminal_result.does_terminate,
        }
        if terminal_result is not None
        else None
    )
    token_count = 0
    if agent.total_usage is not None:
        token_count = agent.total_usage.input_tokens + agent.total_usage.output_tokens

    tracker.finish(
        messages=list(agent._messages),
        terminal_tool_result=terminal_payload,
        token_count=token_count,
        error=run_error,
    )

    return EphemeralRunResult(
        status="failed" if run_error else "completed",
        error=run_error,
        terminal_result=terminal_result,
        agent_name=agent.agent_name,
        event_count=event_count,
    )
