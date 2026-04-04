"""Structured protocol models for the EphemeralOS frontend backends."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any, Literal

from pydantic import BaseModel, Field

from ephemeralos.api.client import SupportsStreamingMessages
from ephemeralos.state.app_state import AppState
from ephemeralos.bridge.manager import BridgeSessionRecord
from ephemeralos.mcp.types import McpConnectionStatus
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

    type: Literal["submit_line", "permission_response", "question_response", "list_sessions", "update_config", "shutdown"]
    line: str | None = None
    request_id: str | None = None
    allowed: bool | None = None
    answer: str | None = None
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
        "modal_request",
        "select_request",
        "error",
        "shutdown",
    ]
    select_options: list[dict[str, Any]] | None = None
    message: str | None = None
    item: TranscriptItem | None = None
    state: dict[str, Any] | None = None
    tasks: list[TaskSnapshot] | None = None
    toolkits: list[ToolkitSnapshot] | None = None
    mcp_servers: list[dict[str, Any]] | None = None
    bridge_sessions: list[dict[str, Any]] | None = None
    commands: list[str] | None = None
    modal: dict[str, Any] | None = None
    tool_name: str | None = None
    tool_input: dict[str, Any] | None = None
    output: str | None = None
    is_error: bool | None = None

    @classmethod
    def ready(
        cls,
        state: AppState,
        tasks: list[TaskRecord],
        commands: list[str],
        toolkits: list[ToolkitSnapshot] | None = None,
    ) -> "BackendEvent":
        return cls(
            type="ready",
            state=_state_payload(state),
            tasks=[TaskSnapshot.from_record(task) for task in tasks],
            toolkits=toolkits or [],
            mcp_servers=[],
            bridge_sessions=[],
            commands=commands,
        )

    @classmethod
    def state_snapshot(cls, state: AppState) -> "BackendEvent":
        return cls(type="state_snapshot", state=_state_payload(state))

    @classmethod
    def tasks_snapshot(cls, tasks: list[TaskRecord]) -> "BackendEvent":
        return cls(
            type="tasks_snapshot",
            tasks=[TaskSnapshot.from_record(task) for task in tasks],
        )

    @classmethod
    def status_snapshot(
        cls,
        *,
        state: AppState,
        mcp_servers: list[McpConnectionStatus],
        bridge_sessions: list[BridgeSessionRecord],
        toolkits: list[ToolkitSnapshot] | None = None,
    ) -> "BackendEvent":
        return cls(
            type="state_snapshot",
            state=_state_payload(state),
            toolkits=toolkits,
            mcp_servers=[
                {
                    "name": server.name,
                    "state": server.state,
                    "detail": server.detail,
                    "transport": server.transport,
                    "auth_configured": server.auth_configured,
                    "tool_count": len(server.tools),
                    "resource_count": len(server.resources),
                }
                for server in mcp_servers
            ],
            bridge_sessions=[
                {
                    "session_id": session.session_id,
                    "command": session.command,
                    "cwd": session.cwd,
                    "pid": session.pid,
                    "status": session.status,
                    "started_at": session.started_at,
                    "output_path": session.output_path,
                }
                for session in bridge_sessions
            ],
        )


def _state_payload(state: AppState) -> dict[str, Any]:
    return {
        "model": state.model,
        "cwd": state.cwd,
        "provider": state.provider,
        "auth_status": state.auth_status,
        "base_url": state.base_url,
        "theme": state.theme,
        "fast_mode": state.fast_mode,
        "effort": state.effort,
        "passes": state.passes,
        "mcp_connected": state.mcp_connected,
        "mcp_failed": state.mcp_failed,
        "bridge_sessions": state.bridge_sessions,
    }


__all__ = [
    "BackendEvent",
    "BackendHostConfig",
    "FrontendRequest",
    "TaskSnapshot",
    "ToolkitSnapshot",
    "TranscriptItem",
]
