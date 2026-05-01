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


# A growing taxonomy of block kinds — kept open via plain str fields rather
# than a closed enum so new recipes can introduce kinds without touching this
# module. The constants below are convenience handles for callers that want
# to avoid stringly-typed code.
class ContextBlockKind(StrEnum):
    """Convenience handles for known kinds. Recipes may use any string."""

    COMPLEX_TASK_GOAL = "complex_task_goal"
    SEGMENT_GOAL = "segment_goal"
    PRIOR_SEGMENT_SPECIFICATION = "prior_segment_specification"
    PRIOR_SEGMENT_SUMMARY = "prior_segment_summary"
    FAILED_GRAPH_LANDSCAPE = "failed_graph_landscape"
    PLANNED_TASK_SPEC = "planned_task_spec"
    TASK_SPECIFICATION = "task_specification"
    EVALUATION_CRITERIA = "evaluation_criteria"
    DEPENDENCY_SUMMARY = "dependency_summary"
    COMPLETED_TASK_SUMMARY = "completed_task_summary"
    ARTIFACT_REFERENCE = "artifact_reference"
    ENTRY_REQUEST = "entry_request"
    PARENT_QUESTION = "parent_question"
    CAPABILITY_NOTE = "capability_note"


class ContextRefs(BaseModel):
    """Canonical row references attached to every packet.

    ``request_id`` is always present; the rest depend on the recipe scope.
    """

    request_id: str
    segment_id: str | None = None
    harness_graph_id: str | None = None
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
            raise ValueError(
                "ContextBlock with priority=required must have non-blank text"
            )
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
