"""ContextPacket persistence store.

Packets are write-once: there is no ``update`` method. Helpers (advisor,
resolver) fetch by id to inherit the parent's frame.
"""

from __future__ import annotations

from db.models.context_packet import ContextPacketRecord
from db.stores.base import SyncStoreMixin
from task_center.context_engine.packet import ContextPacket


class ContextPacketStore(SyncStoreMixin):
    """CRUD for :class:`ContextPacket`. Returns frozen pydantic instances."""

    def insert(self, packet: ContextPacket) -> str:
        """Persist *packet*. Returns the stored id (matches ``packet.id``).

        Re-inserting the same id is rejected at the database layer; callers
        rely on composer-minted UUIDs to keep this simple.
        """
        with self._sf() as db:
            record = ContextPacketRecord(
                id=packet.id,
                target_role=packet.target_role,
                target_id=packet.target_id,
                canonical_refs=packet.canonical_refs.model_dump(mode="json"),
                blocks=[b.model_dump(mode="json") for b in packet.blocks],
                metadata_payload=dict(packet.metadata),
                source_ids=list(packet.source_ids),
            )
            db.add(record)
            db.commit()
        return packet.id

    def get(self, context_packet_id: str) -> ContextPacket | None:
        with self._sf() as db:
            record = db.get(ContextPacketRecord, context_packet_id)
            return self._to_dto(record) if record is not None else None

    @staticmethod
    def _to_dto(record: ContextPacketRecord) -> ContextPacket:
        return ContextPacket.model_validate(
            {
                "id": record.id,
                "target_role": record.target_role,
                "target_id": record.target_id,
                "canonical_refs": record.canonical_refs or {},
                "blocks": list(record.blocks or ()),
                "metadata": dict(record.metadata_payload or {}),
                "source_ids": list(record.source_ids or ()),
            }
        )
