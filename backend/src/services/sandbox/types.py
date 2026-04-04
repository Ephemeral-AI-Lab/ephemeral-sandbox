"""Data models for sandbox lifecycle management."""

from __future__ import annotations

from enum import Enum
from typing import Any

from pydantic import BaseModel, Field


class SandboxState(str, Enum):
    """Possible sandbox states."""

    UNKNOWN = "unknown"
    CREATING = "creating"
    STARTING = "starting"
    STARTED = "started"
    STOPPING = "stopping"
    STOPPED = "stopped"
    ERROR = "error"
    DELETING = "deleting"


class SandboxInfo(BaseModel):
    """Serialised sandbox representation."""

    id: str
    name: str = ""
    state: SandboxState = SandboxState.UNKNOWN
    image: str = ""
    snapshot: str = ""
    project_dir: str = ""
    labels: dict[str, str] = Field(default_factory=dict)
    metadata: dict[str, Any] = Field(default_factory=dict)

    @classmethod
    def from_sdk(cls, sandbox: Any) -> SandboxInfo:
        """Build from a Daytona SDK sandbox object."""
        state_str = getattr(sandbox, "state", "unknown")
        try:
            state = SandboxState(state_str.lower() if isinstance(state_str, str) else "unknown")
        except ValueError:
            state = SandboxState.UNKNOWN

        labels = {}
        raw_labels = getattr(sandbox, "labels", None)
        if isinstance(raw_labels, dict):
            labels = raw_labels
        elif raw_labels is not None:
            # SDK may expose labels as an object with attributes
            for key in dir(raw_labels):
                if not key.startswith("_"):
                    labels[key] = str(getattr(raw_labels, key, ""))

        return cls(
            id=getattr(sandbox, "id", ""),
            name=getattr(sandbox, "name", "") or getattr(sandbox, "id", ""),
            state=state,
            image=labels.get("ephemeralos_image", ""),
            snapshot=labels.get("ephemeralos_snapshot", ""),
            project_dir=getattr(sandbox, "project_dir", "") or labels.get("project_dir", ""),
            labels=labels,
        )


class SandboxHealthResponse(BaseModel):
    """Daytona availability and configuration status."""

    available: bool = False
    api_url: str = ""
    target: str = ""
    error: str = ""
    sandbox_count: int = 0


class CreateSandboxRequest(BaseModel):
    """Payload for sandbox creation."""

    name: str = ""
    image: str = Field(default="", description="Docker image to use for the sandbox")
    snapshot: str = Field(default="", description="Daytona snapshot ID to create from")
    project_dir: str = Field(default="", description="Override workspace root directory")
    labels: dict[str, str] = Field(default_factory=dict)
    env_vars: dict[str, str] = Field(default_factory=dict)
    auto_start: bool = True


class SnapshotInfo(BaseModel):
    """Available Daytona snapshot."""

    id: str
    name: str = ""
    created_at: str = ""
    size: str = ""
