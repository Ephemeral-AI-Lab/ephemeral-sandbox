"""Structured protocol models for the EphemeralOS frontend backends."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any, Literal

from pydantic import BaseModel

from providers.types import SupportsStreamingMessages


@dataclass(frozen=True)
class BackendHostConfig:
    """Configuration for one backend host session."""

    model: str | None = None
    base_url: str | None = None
    system_prompt: str | None = None
    api_key: str | None = None
    api_format: str | None = None
    api_client: SupportsStreamingMessages | None = None
    restore_messages: list[dict] | None = None


class FrontendRequest(BaseModel):
    """One request sent from the React frontend to the Python backend."""

    type: Literal["submit_line", "list_sessions", "update_config", "shutdown"]
    line: str | None = None
    config: dict[str, Any] | None = None


class TranscriptItem(BaseModel):
    """One transcript row rendered by the frontend."""

    role: Literal["system", "user", "assistant", "tool", "tool_result", "log", "thinking"]
    text: str
    tool_name: str | None = None
    tool_input: dict[str, Any] | None = None
    is_error: bool | None = None


class ToolkitSnapshot(BaseModel):
    """UI-safe toolkit representation."""

    name: str
    description: str
    tools: list[str]


class BackendEvent(BaseModel):
    """One event sent from the Python backend to the React frontend."""

    type: Literal[
        "ready",
        "state_snapshot",
        "transcript_item",
        "thinking_delta",
        "assistant_delta",
        "assistant_complete",
        "line_complete",
        "tool_started",
        "tool_completed",
        "tool_cancelled",
        "clear_transcript",
        "error",
        "shutdown",
    ]
    message: str | None = None
    item: TranscriptItem | None = None
    state: dict[str, Any] | None = None
    toolkits: list[ToolkitSnapshot] | None = None
    tool_name: str | None = None
    tool_input: dict[str, Any] | None = None
    output: str | None = None
    is_error: bool | None = None
    cancel_reason: str | None = None

    @classmethod
    def ready(
        cls,
        toolkits: list[ToolkitSnapshot] | None = None,
        state: dict[str, Any] | None = None,
    ) -> "BackendEvent":
        return cls(
            type="ready",
            toolkits=toolkits or [],
            state=state,
        )


__all__ = [
    "BackendEvent",
    "BackendHostConfig",
    "FrontendRequest",
    "ToolkitSnapshot",
    "TranscriptItem",
]
