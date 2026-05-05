"""TaskSegment persistence store. Returns frozen DTOs."""

from __future__ import annotations

import uuid
from datetime import UTC, datetime

from db.models.task_segment import TaskSegmentRecord
from db.stores.base import SyncStoreMixin
from task_center.episode.episode import (
    TaskSegment,
    TaskSegmentCreationReason,
    TaskSegmentStatus,
)


class TaskSegmentStore(SyncStoreMixin):
    """CRUD for TaskSegment. Returns frozen TaskSegment DTOs."""

    def insert(
        self,
        *,
        complex_task_request_id: str,
        sequence_no: int,
        creation_reason: TaskSegmentCreationReason,
        goal: str,
        attempt_budget: int,
    ) -> TaskSegment:
        with self._sf() as db:
            now = datetime.now(UTC)
            record = TaskSegmentRecord(
                id=str(uuid.uuid4()),
                complex_task_request_id=complex_task_request_id,
                sequence_no=sequence_no,
                creation_reason=creation_reason.value,
                goal=goal,
                attempt_budget=attempt_budget,
                status=TaskSegmentStatus.OPEN.value,
                harness_graph_ids=[],
                continuation_goal=None,
                created_at=now,
                updated_at=now,
            )
            db.add(record)
            db.commit()
            db.refresh(record)
            return self._to_dto(record)

    def get(self, segment_id: str) -> TaskSegment | None:
        with self._sf() as db:
            record = db.get(TaskSegmentRecord, segment_id)
            return self._to_dto(record) if record is not None else None

    def append_graph_id(self, segment_id: str, graph_id: str) -> TaskSegment:
        with self._sf() as db:
            record = db.get(TaskSegmentRecord, segment_id)
            if record is None:
                raise LookupError(f"TaskSegment {segment_id!r} not found")
            ids = list(record.harness_graph_ids or [])
            ids.append(graph_id)
            record.harness_graph_ids = ids
            db.commit()
            db.refresh(record)
            return self._to_dto(record)

    def set_continuation_goal(
        self, segment_id: str, continuation_goal: str | None
    ) -> TaskSegment:
        with self._sf() as db:
            record = db.get(TaskSegmentRecord, segment_id)
            if record is None:
                raise LookupError(f"TaskSegment {segment_id!r} not found")
            record.continuation_goal = continuation_goal
            db.commit()
            db.refresh(record)
            return self._to_dto(record)

    def set_status(
        self,
        segment_id: str,
        *,
        status: TaskSegmentStatus,
        closed_at: datetime | None = None,
    ) -> TaskSegment:
        with self._sf() as db:
            record = db.get(TaskSegmentRecord, segment_id)
            if record is None:
                raise LookupError(f"TaskSegment {segment_id!r} not found")
            record.status = status.value
            if closed_at is not None:
                record.closed_at = closed_at
            db.commit()
            db.refresh(record)
            return self._to_dto(record)

    def list_for_request(
        self, complex_task_request_id: str
    ) -> list[TaskSegment]:
        """Ordered by sequence_no ascending."""
        with self._sf() as db:
            q = (
                db.query(TaskSegmentRecord)
                .filter(
                    TaskSegmentRecord.complex_task_request_id
                    == complex_task_request_id
                )
                .order_by(TaskSegmentRecord.sequence_no.asc())
            )
            return [self._to_dto(r) for r in q.all()]

    def list_for_requests(
        self, complex_task_request_ids: list[str]
    ) -> list[TaskSegment]:
        """Ordered by request id, then sequence_no ascending."""
        if not complex_task_request_ids:
            return []
        with self._sf() as db:
            q = (
                db.query(TaskSegmentRecord)
                .filter(
                    TaskSegmentRecord.complex_task_request_id.in_(
                        complex_task_request_ids
                    )
                )
                .order_by(
                    TaskSegmentRecord.complex_task_request_id.asc(),
                    TaskSegmentRecord.sequence_no.asc(),
                )
            )
            return [self._to_dto(r) for r in q.all()]

    def cancel_for_compensation(
        self, segment_id: str, *, closed_at: datetime | None = None
    ) -> TaskSegment:
        """Mark a segment CANCELLED. Reserved for handoff compensation paths."""
        with self._sf() as db:
            record = db.get(TaskSegmentRecord, segment_id)
            if record is None:
                raise LookupError(f"TaskSegment {segment_id!r} not found")
            record.status = TaskSegmentStatus.CANCELLED.value
            if closed_at is not None:
                record.closed_at = closed_at
            db.commit()
            db.refresh(record)
            return self._to_dto(record)

    def get_by_sequence(
        self, *, complex_task_request_id: str, sequence_no: int
    ) -> TaskSegment | None:
        with self._sf() as db:
            record = (
                db.query(TaskSegmentRecord)
                .filter(
                    TaskSegmentRecord.complex_task_request_id
                    == complex_task_request_id,
                    TaskSegmentRecord.sequence_no == sequence_no,
                )
                .first()
            )
            return self._to_dto(record) if record is not None else None

    def close_succeeded(
        self,
        segment_id: str,
        *,
        task_specification: str,
        task_summary: str,
        closed_at: datetime | None = None,
    ) -> TaskSegment:
        """Atomically transition to SUCCEEDED + write denormalized fields.

        All three writes (status, task_specification, task_summary) happen
        inside one ``db.commit()`` so a mid-write crash leaves the row
        untouched. Continuation-segment spawn happens *after* this returns
        and reads the just-closed row's denormalized fields.
        """
        with self._sf() as db:
            record = db.get(TaskSegmentRecord, segment_id)
            if record is None:
                raise LookupError(f"TaskSegment {segment_id!r} not found")
            record.status = TaskSegmentStatus.SUCCEEDED.value
            record.task_specification = task_specification
            record.task_summary = task_summary
            if closed_at is not None:
                record.closed_at = closed_at
            db.commit()
            db.refresh(record)
            return self._to_dto(record)

    def _to_dto(self, record: TaskSegmentRecord) -> TaskSegment:
        return TaskSegment(
            id=record.id,
            complex_task_request_id=record.complex_task_request_id,
            sequence_no=record.sequence_no,
            creation_reason=TaskSegmentCreationReason(record.creation_reason),
            goal=record.goal,
            attempt_budget=record.attempt_budget,
            status=TaskSegmentStatus(record.status),
            harness_graph_ids=tuple(record.harness_graph_ids or ()),
            continuation_goal=record.continuation_goal,
            created_at=record.created_at,
            updated_at=record.updated_at,
            closed_at=record.closed_at,
            task_specification=record.task_specification,
            task_summary=record.task_summary,
        )
