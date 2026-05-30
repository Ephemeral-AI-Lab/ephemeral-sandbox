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

If the agent exits without delivering a terminal tool result — either by
the loop hitting the hard ceiling
(``TERMINAL_NOT_SUBMITTED`` when ``tool_calls_used >=
ceil(1.5 * tool_call_limit)``) or by an unhandled exception —
``terminal_result`` is ``None`` and ``status`` is ``failed``. The loop
itself nudges the agent toward a terminal submission via the
``terminal_call_reminder`` notification rule.
"""

from __future__ import annotations

import logging
from collections.abc import Awaitable, Callable
from dataclasses import dataclass
from typing import TYPE_CHECKING, Any, Literal

from agents import AgentDefinition
from message.message import Message, ToolResultBlock
from message.events import StreamEvent, ToolExecutionCompletedEvent
from tools import ExecutionMetadata, ToolResult

if TYPE_CHECKING:
    from runtime.app_factory import RuntimeConfig

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
    tool_call_count: int


def _last_terminal_tool_result(
    messages: list[Message],
) -> ToolResult | None:
    """Walk *messages* backwards for the last terminating tool result.

    Identifies the result the engine stamped with ``is_terminal=True`` when
    a ``is_terminal_tool=True`` tool returned non-error. Returns the
    corresponding :class:`ToolResult` (with
    ``output``, ``metadata``, etc.) or ``None`` if the loop exited without a
    terminal call (e.g. ``TERMINAL_NOT_SUBMITTED``).
    """
    for msg in reversed(messages):
        if msg.role != "user":
            continue
        for block in reversed(msg.content):
            if isinstance(block, ToolResultBlock) and block.is_terminal:
                return ToolResult(
                    output=str(block.content),
                    is_error=block.is_error,
                    metadata=dict(block.metadata or {}),
                    is_terminal=True,
                )
    return None


async def run_ephemeral_agent(
    config: RuntimeConfig,
    prompt: str,
    *,
    agent_def: AgentDefinition | None = None,
    sandbox_id: str | None = None,
    initial_messages: list[Message] | None = None,
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

    Terminal tools end the run immediately on success. When the agent exits
    without delivering a terminal result the lifecycle returns
    ``status='failed'`` with ``terminal_result=None``; the
    ``terminal_call_reminder`` notification rule handles in-band nudges
    until the hard ceiling ``ceil(1.5 * tool_call_limit)`` is hit.
    Crashes propagate the exception message via ``error`` (also
    ``status='failed'``).
    """
    from engine.agent.run_tracker import AgentRunTracker
    from engine.agent.factory import spawn_agent

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
        task_id=task_id if persist_agent_run else None,
        agent_name=agent.agent_name,
    )
    agent_run_id = tracker.agent_run_id

    if agent.query_context.tool_metadata is None:
        agent.query_context.tool_metadata = ExecutionMetadata()
    if extra_tool_metadata:
        agent.query_context.tool_metadata.update(extra_tool_metadata)
    if task_id:
        agent.query_context.task_center_task_id = task_id
        agent.query_context.tool_metadata.task_center_task_id = task_id
    if agent_run_id is not None:
        agent.query_context.tool_metadata.agent_run_id = agent_run_id
    agent.query_context.run_id = task_id or agent_run_id or agent.query_context.run_id

    tool_call_count = 0
    run_error: str | None = None
    terminal_result: ToolResult | None = None

    try:
        try:
            async for event in agent.run(prompt, auto_close=False):
                if isinstance(event, ToolExecutionCompletedEvent):
                    tool_call_count += 1
                if (
                    isinstance(event, ToolExecutionCompletedEvent)
                    and event.is_terminal
                    and not event.is_error
                ):
                    terminal_result = ToolResult(
                        output=event.output,
                        is_error=event.is_error,
                        metadata=dict(event.metadata or {}),
                        is_terminal=True,
                    )
                if on_event is not None:
                    await on_event(event)
        except Exception as exc:
            run_error = str(exc)
            logger.exception("run_ephemeral_agent: agent run crashed")
    finally:
        close = getattr(agent, "close", None)
        if close is not None:
            try:
                await close()
            except Exception:
                logger.debug("run_ephemeral_agent: agent.close raised", exc_info=True)
        if not run_error and terminal_result is None:
            terminal_result = agent.query_context.terminal_result or _last_terminal_tool_result(
                agent.messages
            )
        if run_error:
            terminal_result = None
        terminal_payload = (
            {
                "output": terminal_result.output,
                "is_error": terminal_result.is_error,
                "metadata": terminal_result.metadata,
                "is_terminal": terminal_result.is_terminal,
            }
            if terminal_result is not None
            else None
        )
        token_count = 0
        if agent.total_usage is not None:
            token_count = agent.total_usage.input_tokens + agent.total_usage.output_tokens

        tracker.finish(
            messages=list(agent.messages),
            terminal_tool_result=terminal_payload,
            token_count=token_count,
            error=run_error,
        )

    return EphemeralRunResult(
        status="failed" if run_error else "completed",
        error=run_error,
        terminal_result=terminal_result,
        agent_name=agent.agent_name,
        tool_call_count=tool_call_count,
    )
