"""Typed packet schema (Phase-06 §"Suggested block kinds").

A :class:`ContextPacket` is the build output of a :class:`ContextRecipe`. It
is the *only* type that travels between the engine and the renderer; every
recipe-side concern (priority, source provenance, per-block metadata) is
serialized here.
"""

from __future__ import annotations

import uuid
from enum import StrEnum
from typing import Any

from pydantic import BaseModel, ConfigDict, Field, field_validator


class ContextPriority(StrEnum):
    """Block-level priority. Token budgets compress lower priorities first."""

    REQUIRED = "required"
    HIGH = "high"
    MEDIUM = "medium"
    LOW = "low"


# ``ContextBlock.kind`` is typed as ``str`` (not this enum) so new recipes can
# introduce kinds without touching this module. The enum below is a namespaced
# set of constants for callers that want to avoid stringly-typed code.
class ContextBlockKind(StrEnum):
    """Convenience constants for known kinds. ``ContextBlock.kind`` accepts any string."""

    GOAL_STATEMENT = "goal_statement"
    ITERATION_STATEMENT = "iteration_statement"
    PRIOR_ITERATION_SUMMARY = "prior_iteration_summary"
    FAILED_ATTEMPT = "failed_attempt"
    PLANNED_TASK_SPEC = "planned_task_spec"
    TASK_SPECIFICATION = "task_specification"
    DEPENDENCY_SUMMARY = "dependency_summary"
    ENTRY_REQUEST = "entry_request"


class ContextRefs(BaseModel):
    """Canonical row references attached to every packet."""

    workflow_id: str | None = None
    iteration_id: str | None = None
    attempt_id: str | None = None
    task_id: str | None = None

    model_config = ConfigDict(extra="forbid")


class ContextBlock(BaseModel):
    """One typed unit of context surfaced to the model."""

    kind: str = Field(min_length=1)
    priority: ContextPriority
    text: str
    source_id: str | None = None
    source_kind: str | None = None
    metadata: dict[str, str] = Field(default_factory=dict)

    model_config = ConfigDict(extra="forbid")

    @field_validator("text")
    @classmethod
    def _non_blank_required_text(cls, value: str, info: Any) -> str:
        priority = info.data.get("priority")
        if priority == ContextPriority.REQUIRED and not value.strip():
            raise ValueError("ContextBlock with priority=required must have non-blank text")
        return value


class ContextPacket(BaseModel):
    """A packet is the immutable output of a recipe build."""

    id: str = Field(default_factory=lambda: str(uuid.uuid4()))
    target_role: str
    target_id: str | None = None
    canonical_refs: ContextRefs
    blocks: list[ContextBlock] = Field(default_factory=list)
    metadata: dict[str, str] = Field(default_factory=dict)
    source_ids: list[str] = Field(default_factory=list)

    model_config = ConfigDict(extra="forbid")
