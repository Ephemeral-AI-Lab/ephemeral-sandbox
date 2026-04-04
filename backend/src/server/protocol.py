"""Structured protocol models for the EphemeralOS frontend backends."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any, Literal

from pydantic import BaseModel, Field

from ephemeralos.models.types import SupportsStreamingMessages
from ephemeralos.tasks.types import TaskRecord


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

    role: Literal["system", "user", "assistant", "tool", "tool_result", "log"]
    text: str
    tool_name: str | None = None
    tool_input: dict[str, Any] | None = None
    is_error: bool | None = None


class TaskSnapshot(BaseModel):
    """UI-safe task representation."""

    id: str
    type: str
    status: str
    description: str
    metadata: dict[str, str] = Field(default_factory=dict)

    @classmethod
    def from_record(cls, record: TaskRecord) -> "TaskSnapshot":
        return cls(
            id=record.id,
            type=record.type,
            status=record.status,
            description=record.description,
            metadata=dict(record.metadata),
        )


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
        "tasks_snapshot",
        "transcript_item",
        "assistant_delta",
        "assistant_complete",
        "line_complete",
        "tool_started",
        "tool_completed",
        "clear_transcript",
        "error",
        "shutdown",
    ]
    message: str | None = None
    item: TranscriptItem | None = None
    state: dict[str, Any] | None = None
    tasks: list[TaskSnapshot] | None = None
    toolkits: list[ToolkitSnapshot] | None = None
    tool_name: str | None = None
    tool_input: dict[str, Any] | None = None
    output: str | None = None
    is_error: bool | None = None

    @classmethod
    def ready(
        cls,
        tasks: list[TaskRecord],
        toolkits: list[ToolkitSnapshot] | None = None,
        state: dict[str, Any] | None = None,
    ) -> "BackendEvent":
        return cls(
            type="ready",
            tasks=[TaskSnapshot.from_record(task) for task in tasks],
            toolkits=toolkits or [],
            state=state,
        )

    @classmethod
    def tasks_snapshot(cls, tasks: list[TaskRecord]) -> "BackendEvent":
        return cls(
            type="tasks_snapshot",
            tasks=[TaskSnapshot.from_record(task) for task in tasks],
        )


__all__ = [
    "BackendEvent",
    "BackendHostConfig",
    "FrontendRequest",
    "TaskSnapshot",
    "ToolkitSnapshot",
    "TranscriptItem",
]
