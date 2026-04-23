"""Pydantic schemas for agent definition CRUD endpoints."""

from __future__ import annotations

from datetime import datetime
from typing import Any

from pydantic import BaseModel, Field, field_validator

from agents.types import EFFORT_LEVELS


class AgentDefinitionCreate(BaseModel):
    name: str = Field(min_length=1, max_length=128)
    description: str = Field(min_length=1)
    system_prompt: str | None = None
    model: str = Field(
        min_length=1, description="Model key — each agent must be tied to a registered model key"
    )
    effort: str | None = None
    tool_call_limit: int | None = Field(default=None, gt=0)
    toolkits: list[str] | None = None
    skills: list[str] = Field(default_factory=list)
    allowed_tools: list[str] = Field(default_factory=list)
    blocked_tools: list[str] = Field(default_factory=list)
    hooks: dict[str, Any] | None = None
    background: bool = False
    initial_prompt: str | None = None
    tags: list[str] | None = None
    metadata: dict[str, Any] | None = None
    created_by: str | None = None

    @field_validator("effort")
    @classmethod
    def check_effort(cls, v: str | None) -> str | None:
        if v is not None and v not in EFFORT_LEVELS:
            raise ValueError(f"effort must be one of {EFFORT_LEVELS}")
        return v


class AgentDefinitionUpdate(BaseModel):
    description: str | None = None
    system_prompt: str | None = None
    model: str | None = Field(default=None, description="Model key override")
    effort: str | None = None
    tool_call_limit: int | None = Field(default=None, gt=0)
    toolkits: list[str] | None = None
    skills: list[str] | None = None
    allowed_tools: list[str] | None = None
    blocked_tools: list[str] | None = None
    hooks: dict[str, Any] | None = None
    background: bool | None = None
    initial_prompt: str | None = None
    tags: list[str] | None = None
    metadata: dict[str, Any] | None = None

    @field_validator("effort")
    @classmethod
    def check_effort(cls, v: str | None) -> str | None:
        if v is not None and v not in EFFORT_LEVELS:
            raise ValueError(f"effort must be one of {EFFORT_LEVELS}")
        return v


class AgentDefinitionResponse(BaseModel):
    model_config = {"from_attributes": True}

    id: str
    name: str
    description: str
    system_prompt: str | None = None
    model: str
    effort: str | None = None
    tool_call_limit: int | None = None
    toolkits: list[str] | None = None
    skills: list[str] = Field(default_factory=list)
    allowed_tools: list[str] = Field(default_factory=list)
    blocked_tools: list[str] = Field(default_factory=list)
    hooks: dict[str, Any] | None = None
    background: bool = False
    initial_prompt: str | None = None
    version: int = 1
    is_active: bool = True
    created_by: str | None = None
    tags: list[str] | None = None
    metadata_json: dict[str, Any] | None = Field(default=None, alias="metadata")
    created_at: datetime
    updated_at: datetime


class AgentValidationResult(BaseModel):
    valid: bool
    errors: list[str] = Field(default_factory=list)
    warnings: list[str] = Field(default_factory=list)


class CloneRequest(BaseModel):
    new_name: str = Field(min_length=1, max_length=128)
