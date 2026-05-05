"""ComplexTaskRequest persistence store. Returns frozen DTOs."""

from __future__ import annotations

import uuid
from datetime import UTC, datetime

from db.models.complex_task_request import ComplexTaskRequestRecord
from db.stores.base import SyncStoreMixin
from task_center.mission.mission import (
    ComplexTaskRequest,
    ComplexTaskRequestStatus,
)


class ComplexTaskRequestStore(SyncStoreMixin):
    """CRUD for ComplexTaskRequest. Returns frozen ComplexTaskRequest DTOs."""

    def insert(
        self,
        *,
        task_center_run_id: str,
        requested_by_task_id: str,
        goal: str,
    ) -> ComplexTaskRequest:
        with self._sf() as db:
            now = datetime.now(UTC)
            record = ComplexTaskRequestRecord(
                id=str(uuid.uuid4()),
                task_center_run_id=task_center_run_id,
                requested_by_task_id=requested_by_task_id,
                goal=goal,
                status=ComplexTaskRequestStatus.OPEN.value,
                task_segment_ids=[],
                final_outcome=None,
                created_at=now,
                updated_at=now,
            )
            db.add(record)
            db.commit()
            db.refresh(record)
            return self._to_dto(record)

    def get(self, request_id: str) -> ComplexTaskRequest | None:
        with self._sf() as db:
            record = db.get(ComplexTaskRequestRecord, request_id)
            return self._to_dto(record) if record is not None else None

    def append_segment_id(
        self, request_id: str, segment_id: str
    ) -> ComplexTaskRequest:
        with self._sf() as db:
            record = db.get(ComplexTaskRequestRecord, request_id)
            if record is None:
                raise LookupError(f"ComplexTaskRequest {request_id!r} not found")
            ids = list(record.task_segment_ids or [])
            ids.append(segment_id)
            record.task_segment_ids = ids
            db.commit()
            db.refresh(record)
            return self._to_dto(record)

    def set_status(
        self,
        request_id: str,
        *,
        status: ComplexTaskRequestStatus,
        final_outcome: dict | None,
        closed_at: datetime | None = None,
    ) -> ComplexTaskRequest:
        with self._sf() as db:
            record = db.get(ComplexTaskRequestRecord, request_id)
            if record is None:
                raise LookupError(f"ComplexTaskRequest {request_id!r} not found")
            record.status = status.value
            record.final_outcome = final_outcome
            if closed_at is not None:
                record.closed_at = closed_at
            db.commit()
            db.refresh(record)
            return self._to_dto(record)

    def list_for_executor_task(
        self, requested_by_task_id: str
    ) -> list[ComplexTaskRequest]:
        with self._sf() as db:
            q = (
                db.query(ComplexTaskRequestRecord)
                .filter(
                    ComplexTaskRequestRecord.requested_by_task_id
                    == requested_by_task_id
                )
                .order_by(ComplexTaskRequestRecord.created_at.asc())
            )
            return [self._to_dto(r) for r in q.all()]

    def list_for_run(
        self, task_center_run_id: str
    ) -> list[ComplexTaskRequest]:
        with self._sf() as db:
            q = (
                db.query(ComplexTaskRequestRecord)
                .filter(
                    ComplexTaskRequestRecord.task_center_run_id
                    == task_center_run_id
                )
                .order_by(ComplexTaskRequestRecord.created_at.asc())
            )
            return [self._to_dto(r) for r in q.all()]

    def cancel_for_compensation(
        self, request_id: str, *, closed_at: datetime | None = None
    ) -> ComplexTaskRequest:
        """Mark a request CANCELLED. Reserved for handoff compensation paths."""
        with self._sf() as db:
            record = db.get(ComplexTaskRequestRecord, request_id)
            if record is None:
                raise LookupError(f"ComplexTaskRequest {request_id!r} not found")
            record.status = ComplexTaskRequestStatus.CANCELLED.value
            if closed_at is not None:
                record.closed_at = closed_at
            db.commit()
            db.refresh(record)
            return self._to_dto(record)

    def _to_dto(self, record: ComplexTaskRequestRecord) -> ComplexTaskRequest:
        return ComplexTaskRequest(
            id=record.id,
            task_center_run_id=record.task_center_run_id,
            requested_by_task_id=record.requested_by_task_id,
            goal=record.goal,
            status=ComplexTaskRequestStatus(record.status),
            task_segment_ids=tuple(record.task_segment_ids or ()),
            final_outcome=record.final_outcome,
            created_at=record.created_at,
            updated_at=record.updated_at,
            closed_at=record.closed_at,
        )
