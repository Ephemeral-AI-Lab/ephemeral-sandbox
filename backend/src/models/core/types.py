"""Core model types — request/response dataclasses, streaming protocol, usage tracking."""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Any, AsyncIterator, Protocol

from pydantic import BaseModel

if TYPE_CHECKING:
    from message import ConversationMessage


# ---------------------------------------------------------------------------
# Usage
# ---------------------------------------------------------------------------


class UsageSnapshot(BaseModel):
    """Token usage returned by the model provider."""

    input_tokens: int = 0
    output_tokens: int = 0

    @property
    def total_tokens(self) -> int:
        """Return the total number of accounted tokens."""
        return self.input_tokens + self.output_tokens


# ---------------------------------------------------------------------------
# API message request / stream events
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class ApiMessageRequest:
    """Input parameters for a model invocation."""

    model: str
    messages: list[ConversationMessage]
    system_prompt: str | None = None
    max_tokens: int = 4096
    tools: list[dict[str, Any]] = field(default_factory=list)


@dataclass(frozen=True)
class ApiThinkingDeltaEvent:
    """Incremental thinking/reasoning content from the model."""

    text: str


@dataclass(frozen=True)
class ApiTextDeltaEvent:
    """Incremental text produced by the model."""

    text: str


@dataclass(frozen=True)
class ApiMessageCompleteEvent:
    """Terminal event containing the full assistant message."""

    message: ConversationMessage
    usage: UsageSnapshot
    stop_reason: str | None = None


@dataclass(frozen=True)
class ApiToolUseDeltaEvent:
    """Tool use block arriving mid-stream.

    Emitted when the API streams a tool_use content block before the
    complete message is available. Allows early tool execution start.
    """

    id: str
    name: str
    input: dict[str, Any]


@dataclass(frozen=True)
class ApiCancelEvent:
    """LLM cancel signal for a running tool.

    Emitted when the LLM decides to abort a long-running tool.
    The loop intercepts this and calls executor.cancel().
    """

    tool_id: str
    reason: str


ApiStreamEvent = (
    ApiThinkingDeltaEvent
    | ApiTextDeltaEvent
    | ApiMessageCompleteEvent
    | ApiToolUseDeltaEvent
    | ApiCancelEvent
)


# ---------------------------------------------------------------------------
# Streaming protocol
# ---------------------------------------------------------------------------


class SupportsStreamingMessages(Protocol):
    """Protocol used by the query engine in tests and production."""

    async def stream_message(self, request: ApiMessageRequest) -> AsyncIterator[ApiStreamEvent]:
        """Yield streamed events for the request."""
