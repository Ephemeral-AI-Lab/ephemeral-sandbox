"""US-008: ContextPacketStore round-trip + immutability."""

from __future__ import annotations

import pytest
from sqlalchemy import create_engine
from sqlalchemy.orm import sessionmaker

import db.models  # noqa: F401 - populate Base.metadata
from db.base import Base
from db.stores.context_packet_store import ContextPacketStore
from task_center.context_engine.packet import (
    ContextBlock,
    ContextPacket,
    ContextPriority,
    ContextRefs,
)


@pytest.fixture
def packet_store():
    engine = create_engine("sqlite:///:memory:")
    Base.metadata.create_all(engine)
    sf = sessionmaker(bind=engine, autoflush=False, expire_on_commit=False)
    store = ContextPacketStore()
    store.initialize(sf)
    yield store
    engine.dispose()


def _make_packet(packet_id: str = "pkt-1") -> ContextPacket:
    return ContextPacket(
        id=packet_id,
        target_role="planner",
        target_id="g-1",
        canonical_refs=ContextRefs(
            request_id="req-A", segment_id="seg-1", harness_graph_id="g-1"
        ),
        blocks=[
            ContextBlock(
                kind="segment_goal",
                priority=ContextPriority.REQUIRED,
                text="goal",
                source_id="seg-1",
                source_kind="task_segment",
            ),
            ContextBlock(
                kind="prior_segment_summary",
                priority=ContextPriority.HIGH,
                text="summary",
                source_id="seg-prior",
                source_kind="task_segment",
                metadata={
                    "segment_sequence_no": "1",
                    "inherited_from_parent": "true",
                },
            ),
        ],
        metadata={"is_initial_segment": "false"},
        source_ids=["seg-1", "seg-prior"],
    )


def test_round_trip_preserves_blocks_and_metadata(packet_store):
    packet = _make_packet()
    stored_id = packet_store.insert(packet)
    assert stored_id == packet.id
    loaded = packet_store.get(stored_id)
    assert loaded is not None
    assert loaded.target_role == "planner"
    assert loaded.canonical_refs.request_id == "req-A"
    assert len(loaded.blocks) == 2
    inherited = loaded.blocks[1]
    assert inherited.metadata["segment_sequence_no"] == "1"
    assert inherited.metadata["inherited_from_parent"] == "true"
    assert loaded.metadata["is_initial_segment"] == "false"
    assert loaded.source_ids == ["seg-1", "seg-prior"]


def test_unknown_id_returns_none(packet_store):
    assert packet_store.get("does-not-exist") is None


def test_no_update_method_on_store():
    """Packets are write-once."""
    assert not hasattr(ContextPacketStore, "update")
    assert not hasattr(ContextPacketStore, "set")
