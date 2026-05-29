"""US-002: ContextPacket / ContextBlock schema validation."""

from __future__ import annotations

import pytest
from pydantic import ValidationError

from task_center.context_engine.packet import (
    ContextBlock,
    ContextPacket,
    ContextPriority,
    ContextRefs,
)


def test_block_required_priority_rejects_blank_text():
    with pytest.raises(ValidationError):
        ContextBlock(
            kind="iteration_statement",
            priority=ContextPriority.REQUIRED,
            text="   ",
        )


def test_block_high_priority_accepts_blank_text():
    """Only required blocks enforce non-blank — high/medium/low can carry
    structured placeholders."""
    block = ContextBlock(
        kind="prior_iteration_summary",
        priority=ContextPriority.HIGH,
        text="",
    )
    assert block.text == ""


def test_block_metadata_round_trip():
    block = ContextBlock(
        kind="prior_iteration_specification",
        priority=ContextPriority.HIGH,
        text="spec",
        metadata={"iteration_sequence_no": "2", "source_label": "accepted"},
    )
    assert block.metadata["iteration_sequence_no"] == "2"
    assert block.metadata["source_label"] == "accepted"


def test_packet_auto_generates_id():
    packet = ContextPacket(
        target_role="planner",
        target_id="g-1",
        canonical_refs=ContextRefs(workflow_id="req-A"),
    )
    assert packet.id  # non-empty UUID string
    assert isinstance(packet.id, str)


def test_packet_serialization_round_trip():
    packet = ContextPacket(
        target_role="planner",
        target_id=None,
        canonical_refs=ContextRefs(
            workflow_id="req",
            iteration_id="seg",
            attempt_id="g",
            task_id="t",
        ),
        blocks=[
            ContextBlock(
                kind="iteration_statement",
                priority=ContextPriority.REQUIRED,
                text="goal text",
                source_id="seg",
                source_kind="iteration",
            )
        ],
        metadata={"is_initial_iteration": "true"},
        source_ids=["seg"],
    )
    payload = packet.model_dump()
    restored = ContextPacket.model_validate(payload)
    assert restored.id == packet.id
    assert restored.blocks[0].text == "goal text"
    assert restored.metadata["is_initial_iteration"] == "true"


def test_packet_extra_fields_rejected():
    with pytest.raises(ValidationError):
        ContextPacket(
            target_role="planner",
            canonical_refs=ContextRefs(workflow_id="r"),
            unknown="x",  # type: ignore[arg-type]
        )
