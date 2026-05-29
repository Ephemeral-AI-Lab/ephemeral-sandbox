"""``ScenarioEventSource`` — drive a mock agent through the REAL query loop.

This is the LLM mock. It is injected per-agent via
``RuntimeConfig.event_source_factory`` and assigned to
``QueryContext.event_source`` by ``engine.agent.factory.spawn_agent``. The query
loop then streams from it instead of the provider ``api_client`` — every other
part of the loop (dispatch, terminal-alone enforcement, budget counting,
notification rules) runs unchanged. The mock path is therefore byte-identical to
production except for the event *content* (scripted vs LLM).

Each ``__call__`` corresponds to exactly one loop turn:

1. Read the trailing ``ToolResultBlock``s from the built request (the results of
   the previous turn's tool calls) and feed them to the agent's turn-coroutine.
2. Advance the coroutine to the next :class:`Turn`.
3. Emit one :class:`~message.events.ToolUseDeltaEvent` per tool_use (ids matching
   the complete message's ``ToolUseBlock``s) — REQUIRED for budget parity: the
   loop counts tool dispatches at stream time (``loop.py`` ``_count_tool_dispatch``
   on each delta) and populates ``streamed_tool_use_ids`` so dispatch-time
   ``consume_budget`` gating matches the real provider — then one
   :class:`~message.events.AssistantMessageCompleteEvent` carrying the full
   ``[ThinkingBlock?, TextBlock?, ToolUseBlock...]`` message.

Thinking/text deltas are optional (reporting-only); the complete message already
carries those blocks.
"""

from __future__ import annotations

from collections.abc import AsyncGenerator, AsyncIterator, Callable
from dataclasses import dataclass, field
from typing import TYPE_CHECKING
from uuid import uuid4

from message.events import (
    AssistantMessageCompleteEvent,
    AssistantTextDeltaEvent,
    StreamEvent,
    ThinkingDeltaEvent,
    ToolUseDeltaEvent,
)
from message.message import (
    Message,
    TextBlock,
    ThinkingBlock,
    ToolResultBlock,
    ToolUseBlock,
)
from providers.types import UsageSnapshot

if TYPE_CHECKING:
    from engine.query.context import QueryContext
    from engine.query.request import QueryRunRequest


@dataclass(frozen=True, slots=True)
class ToolCall:
    """One tool the scripted agent calls this turn."""

    name: str
    input: dict = field(default_factory=dict)


@dataclass(frozen=True, slots=True)
class Turn:
    """One scripted assistant turn: optional thinking/text + zero or more calls.

    A terminal tool must be the sole call in its ``Turn`` (the loop rejects a
    batched terminal exactly as it would for a real model).
    """

    calls: tuple[ToolCall, ...] = ()
    thinking: str | None = None
    text: str | None = None


# A turn-coroutine is an async generator that ``yield``s a :class:`Turn` and is
# resumed (``asend``) with the ``list[ToolResultBlock]`` produced by executing
# the previous turn's calls. The first ``asend(None)`` primes it to the first
# ``Turn``.
TurnScript = AsyncGenerator[Turn, "list[ToolResultBlock]"]


def latest_tool_results(run_request: QueryRunRequest) -> list[ToolResultBlock]:
    """The previous turn's ``ToolResultBlock``s from the built request.

    ``run_request.request.messages`` is a ``list[Message]`` (local objects, not
    provider dicts — see ``engine.query.provider_history.build_provider_messages``).
    Scans backward for the most recent user message that carries tool results:
    the loop appends a notification/reminder user message at the top of a turn
    AFTER the prior turn's tool-result user message, so the trailing message is
    not always the tool results.
    """
    for message in reversed(run_request.request.messages):
        if getattr(message, "role", None) != "user":
            continue
        blocks = [b for b in message.content if isinstance(b, ToolResultBlock)]
        if blocks:
            return blocks
    return []


class ScenarioEventSource:
    """Per-agent scripted event source. Holds one live turn-coroutine.

    Construct with either a ready ``script`` (simple/static cases) or a
    ``script_builder`` invoked lazily on the first turn with the live
    ``QueryContext`` — the scenario path needs ``context.tool_metadata``
    (task_id, attempt_runtime) to build the role-appropriate script, and that
    is only available at call time (``spawn_agent`` passes the factory only the
    ``AgentDefinition``).
    """

    def __init__(
        self,
        script: TurnScript | None = None,
        *,
        script_builder: Callable[[QueryContext], TurnScript] | None = None,
        agent_name: str = "",
        run_id: str = "",
    ) -> None:
        if (script is None) == (script_builder is None):
            raise ValueError("Pass exactly one of script / script_builder.")
        self._script = script
        self._script_builder = script_builder
        self._agent_name = agent_name
        self._run_id = run_id
        self._primed = False

    async def __call__(
        self,
        context: QueryContext,
        run_request: QueryRunRequest,
    ) -> AsyncIterator[StreamEvent]:
        if self._script is None:
            assert self._script_builder is not None  # guarded in __init__
            self._script = self._script_builder(context)
        turn = await self._advance(run_request)

        thinking_block = (
            [ThinkingBlock(text=turn.thinking)] if turn.thinking else []
        )
        text_block = [TextBlock(text=turn.text)] if turn.text else []
        tool_use_blocks = [
            ToolUseBlock(
                tool_use_id=f"toolu_{uuid4().hex}",
                name=call.name,
                input=dict(call.input),
            )
            for call in turn.calls
        ]

        if turn.thinking:
            yield ThinkingDeltaEvent(
                text=turn.thinking,
                agent_name=self._agent_name,
                run_id=self._run_id,
            )
        if turn.text:
            yield AssistantTextDeltaEvent(
                text=turn.text,
                agent_name=self._agent_name,
                run_id=self._run_id,
            )
        # One delta per tool_use — required for stream-time budget parity.
        for block in tool_use_blocks:
            yield ToolUseDeltaEvent(
                tool_use_id=block.tool_use_id,
                name=block.name,
                input=block.input,
                agent_name=self._agent_name,
                run_id=self._run_id,
            )
        yield AssistantMessageCompleteEvent(
            message=Message(
                role="assistant",
                content=[*thinking_block, *text_block, *tool_use_blocks],
            ),
            usage=UsageSnapshot(),
            agent_name=self._agent_name,
            run_id=self._run_id,
        )

    async def _advance(self, run_request: QueryRunRequest) -> Turn:
        try:
            if not self._primed:
                self._primed = True
                return await self._script.asend(None)
            return await self._script.asend(latest_tool_results(run_request))
        except StopAsyncIteration:
            # Script ran out of turns without submitting a terminal. End the
            # stream with a text-only assistant message (no tool_uses) — the
            # loop counts this as a text-only no-terminal turn, exactly like a
            # real model that stopped calling tools.
            return Turn()


__all__ = [
    "ScenarioEventSource",
    "ToolCall",
    "Turn",
    "TurnScript",
    "latest_tool_results",
]
