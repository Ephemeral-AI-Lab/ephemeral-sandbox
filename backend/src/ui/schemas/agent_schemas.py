"""Pydantic schemas for agent definition CRUD endpoints."""

from __future__ import annotations

from datetime import datetime
from typing import Any

from pydantic import BaseModel, Field, field_validator

from ephemeralos.coordinator.agent_definitions import (
    AGENT_COLORS,
    EFFORT_LEVELS,
    ISOLATION_MODES,
    MEMORY_SCOPES,
    PERMISSION_MODES,
)


class AgentDefinitionCreate(BaseModel):
    """Request body for creating an agent definition."""

    name: str = Field(min_length=1, max_length=128)
    description: str = Field(min_length=1)
    system_prompt: str | None = None
    model: str | None = None
    effort: str | None = None
    permission_mode: str | None = None
    max_turns: int | None = Field(default=None, gt=0)
    tools: list[str] | None = None
    disallowed_tools: list[str] | None = None
    toolkits: list[str] | None = None
    skills: list[str] = Field(default_factory=list)
    mcp_servers: list[Any] | None = None
    hooks: dict[str, Any] | None = None
    color: str | None = None
    background: bool = False
    initial_prompt: str | None = None
    memory: str | None = None
    isolation: str | None = None
    subagent_type: str = "general-purpose"
    tags: list[str] | None = None
    metadata: dict[str, Any] | None = None
    created_by: str | None = None

    @field_validator("effort")
    @classmethod
    def check_effort(cls, v: str | None) -> str | None:
        if v is not None and v not in EFFORT_LEVELS:
            raise ValueError(f"effort must be one of {EFFORT_LEVELS}")
        return v

    @field_validator("permission_mode")
    @classmethod
    def check_permission_mode(cls, v: str | None) -> str | None:
        if v is not None and v not in PERMISSION_MODES:
            raise ValueError(f"permission_mode must be one of {PERMISSION_MODES}")
        return v

    @field_validator("color")
    @classmethod
    def check_color(cls, v: str | None) -> str | None:
        if v is not None and v not in AGENT_COLORS:
            raise ValueError(f"color must be one of {sorted(AGENT_COLORS)}")
        return v

    @field_validator("memory")
    @classmethod
    def check_memory(cls, v: str | None) -> str | None:
        if v is not None and v not in MEMORY_SCOPES:
            raise ValueError(f"memory must be one of {MEMORY_SCOPES}")
        return v

    @field_validator("isolation")
    @classmethod
    def check_isolation(cls, v: str | None) -> str | None:
        if v is not None and v not in ISOLATION_MODES:
            raise ValueError(f"isolation must be one of {ISOLATION_MODES}")
        return v


class AgentDefinitionUpdate(BaseModel):
    """Partial update — only provided fields are changed."""

    description: str | None = None
    system_prompt: str | None = None
    model: str | None = None
    effort: str | None = None
    permission_mode: str | None = None
    max_turns: int | None = Field(default=None, gt=0)
    tools: list[str] | None = None
    disallowed_tools: list[str] | None = None
    toolkits: list[str] | None = None
    skills: list[str] | None = None
    mcp_servers: list[Any] | None = None
    hooks: dict[str, Any] | None = None
    color: str | None = None
    background: bool | None = None
    initial_prompt: str | None = None
    memory: str | None = None
    isolation: str | None = None
    subagent_type: str | None = None
    tags: list[str] | None = None
    metadata: dict[str, Any] | None = None

    @field_validator("effort")
    @classmethod
    def check_effort(cls, v: str | None) -> str | None:
        if v is not None and v not in EFFORT_LEVELS:
            raise ValueError(f"effort must be one of {EFFORT_LEVELS}")
        return v

    @field_validator("permission_mode")
    @classmethod
    def check_permission_mode(cls, v: str | None) -> str | None:
        if v is not None and v not in PERMISSION_MODES:
            raise ValueError(f"permission_mode must be one of {PERMISSION_MODES}")
        return v

    @field_validator("color")
    @classmethod
    def check_color(cls, v: str | None) -> str | None:
        if v is not None and v not in AGENT_COLORS:
            raise ValueError(f"color must be one of {sorted(AGENT_COLORS)}")
        return v

    @field_validator("memory")
    @classmethod
    def check_memory(cls, v: str | None) -> str | None:
        if v is not None and v not in MEMORY_SCOPES:
            raise ValueError(f"memory must be one of {MEMORY_SCOPES}")
        return v

    @field_validator("isolation")
    @classmethod
    def check_isolation(cls, v: str | None) -> str | None:
        if v is not None and v not in ISOLATION_MODES:
            raise ValueError(f"isolation must be one of {ISOLATION_MODES}")
        return v


class AgentDefinitionResponse(BaseModel):
    """API response for an agent definition."""

    model_config = {"from_attributes": True}

    id: str
    name: str
    description: str
    system_prompt: str | None = None
    model: str | None = None
    effort: str | None = None
    permission_mode: str | None = None
    max_turns: int | None = None
    tools: list[str] | None = None
    disallowed_tools: list[str] | None = None
    toolkits: list[str] | None = None
    skills: list[str] = Field(default_factory=list)
    mcp_servers: list[Any] | None = None
    hooks: dict[str, Any] | None = None
    color: str | None = None
    background: bool = False
    initial_prompt: str | None = None
    memory: str | None = None
    isolation: str | None = None
    subagent_type: str = "general-purpose"
    version: int = 1
    is_active: bool = True
    created_by: str | None = None
    tags: list[str] | None = None
    metadata_json: dict[str, Any] | None = Field(default=None, alias="metadata")
    created_at: datetime
    updated_at: datetime


class AgentValidationResult(BaseModel):
    """Result of validating an agent definition."""

    valid: bool
    errors: list[str] = Field(default_factory=list)
    warnings: list[str] = Field(default_factory=list)


class CloneRequest(BaseModel):
    """Request body for cloning an agent."""

    new_name: str = Field(min_length=1, max_length=128)
