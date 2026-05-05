"""US-005: AgentDefinition variants + context_recipe round-trip."""

from __future__ import annotations

import pytest

from agents.types import (
    AgentDefinition,
    AgentSelectionBlock,
    AgentVariant,
)


def test_variant_round_trips_through_pydantic():
    defn = AgentDefinition(
        name="planner",
        description="planner",
        context_recipe="planner_v1",
        variants=[
            AgentVariant(
                when="partial_plan_caller_ancestor",
                use="planner_full_only",
                note="ancestry contains a partial-planned caller attempt",
                required_context_blocks=[
                    AgentSelectionBlock(
                        kind="launch_notice",
                        priority="required",
                        text="Use the selected terminal surface.",
                    )
                ],
            )
        ],
    )
    payload = defn.model_dump()
    restored = AgentDefinition.model_validate(payload)
    assert restored.context_recipe == "planner_v1"
    assert restored.variants[0].use == "planner_full_only"
    assert restored.variants[0].required_context_blocks[0].kind == "launch_notice"


def test_definition_default_variants_is_empty():
    defn = AgentDefinition(name="x", description="x")
    assert defn.variants == []
    assert defn.context_recipe is None


def test_selection_block_priority_is_validated_at_resolver_time(monkeypatch):
    """The pydantic field accepts any string; ContextPriority enum validation
    happens when the resolver converts it to a real ContextBlock."""
    block = AgentSelectionBlock(
        kind="launch_notice", priority="required", text="t"
    )
    assert block.priority == "required"


def test_variant_extra_fields_rejected():
    with pytest.raises(Exception):
        AgentVariant(
            when="x", use="y", note="", unknown="bad"  # type: ignore[arg-type]
        )
