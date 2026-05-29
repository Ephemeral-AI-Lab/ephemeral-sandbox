"""Iteration persistence store. Returns frozen DTOs."""

from __future__ import annotations

import uuid
from datetime import UTC, datetime

from db.models.iteration import IterationRecord
from db.stores.base import SyncStoreMixin
from task_center.iteration.state import (
    Iteration,
    IterationCreationReason,
    IterationStatus,
)


class IterationStore(SyncStoreMixin):
    """CRUD for Iteration. Returns frozen Iteration DTOs."""

    def insert(
        self,
        *,
        workflow_id: str,
        sequence_no: int,
        creation_reason: IterationCreationReason,
        goal: str,
        attempt_budget: int,
    ) -> Iteration:
        with self._sf() as db:
            now = datetime.now(UTC)
            record = IterationRecord(
                id=str(uuid.uuid4()),
                workflow_id=workflow_id,
                sequence_no=sequence_no,
                creation_reason=creation_reason.value,
                goal=goal,
                attempt_budget=attempt_budget,
                status=IterationStatus.OPEN.value,
                attempt_ids=[],
                deferred_goal=None,
                created_at=now,
                updated_at=now,
            )
            db.add(record)
            db.commit()
            db.refresh(record)
            return self._to_dto(record)

    def get(self, iteration_id: str) -> Iteration | None:
        with self._sf() as db:
            record = db.get(IterationRecord, iteration_id)
            return self._to_dto(record) if record is not None else None

    def append_attempt_id(self, iteration_id: str, attempt_id: str) -> Iteration:
        with self._sf() as db:
            record = db.get(IterationRecord, iteration_id)
            if record is None:
                raise LookupError(f"Iteration {iteration_id!r} not found")
            ids = list(record.attempt_ids or [])
            ids.append(attempt_id)
            record.attempt_ids = ids
            db.commit()
            db.refresh(record)
            return self._to_dto(record)

    def set_deferred_goal_for_next_iteration(
        self, iteration_id: str, deferred_goal_for_next_iteration: str | None
    ) -> Iteration:
        with self._sf() as db:
            record = db.get(IterationRecord, iteration_id)
            if record is None:
                raise LookupError(f"Iteration {iteration_id!r} not found")
            record.deferred_goal = deferred_goal_for_next_iteration
            db.commit()
            db.refresh(record)
            return self._to_dto(record)

    def set_status(
        self,
        iteration_id: str,
        *,
        status: IterationStatus,
        closed_at: datetime | None = None,
    ) -> Iteration:
        with self._sf() as db:
            record = db.get(IterationRecord, iteration_id)
            if record is None:
                raise LookupError(f"Iteration {iteration_id!r} not found")
            record.status = status.value
            if closed_at is not None:
                record.closed_at = closed_at
            db.commit()
            db.refresh(record)
            return self._to_dto(record)

    def list_for_workflow(
        self, workflow_id: str
    ) -> list[Iteration]:
        """Ordered by sequence_no ascending."""
        with self._sf() as db:
            q = (
                db.query(IterationRecord)
                .filter(
                    IterationRecord.workflow_id
                    == workflow_id
                )
                .order_by(IterationRecord.sequence_no.asc())
            )
            return [self._to_dto(r) for r in q.all()]

    def get_by_sequence(
        self, *, workflow_id: str, sequence_no: int
    ) -> Iteration | None:
        with self._sf() as db:
            record = (
                db.query(IterationRecord)
                .filter(
                    IterationRecord.workflow_id
                    == workflow_id,
                    IterationRecord.sequence_no == sequence_no,
                )
                .first()
            )
            return self._to_dto(record) if record is not None else None

    def close_succeeded(
        self,
        iteration_id: str,
        *,
        plan_spec: str,
        task_summary: str,
        closed_at: datetime | None = None,
    ) -> Iteration:
        """Atomically transition to SUCCEEDED + write denormalized fields.

        All three writes (status, plan_spec, task_summary) happen
        inside one ``db.commit()`` so a mid-write crash leaves the row
        untouched. Continuation-segment spawn happens *after* this returns
        and reads the just-closed row's denormalized fields.
        """
        with self._sf() as db:
            record = db.get(IterationRecord, iteration_id)
            if record is None:
                raise LookupError(f"Iteration {iteration_id!r} not found")
            record.status = IterationStatus.SUCCEEDED.value
            # DB column name task_specification pinned by ADR (FU-2 renames the column).
            record.task_specification = plan_spec
            record.task_summary = task_summary
            if closed_at is not None:
                record.closed_at = closed_at
            db.commit()
            db.refresh(record)
            return self._to_dto(record)

    def _to_dto(self, record: IterationRecord) -> Iteration:
        return Iteration(
            id=record.id,
            workflow_id=record.workflow_id,
            sequence_no=record.sequence_no,
            creation_reason=IterationCreationReason(record.creation_reason),
            goal=record.goal,
            attempt_budget=record.attempt_budget,
            status=IterationStatus(record.status),
            attempt_ids=tuple(record.attempt_ids or ()),
            deferred_goal_for_next_iteration=record.deferred_goal,
            created_at=record.created_at,
            updated_at=record.updated_at,
            closed_at=record.closed_at,
            # DB column name task_specification pinned by ADR (FU-2 renames the column).
            plan_spec=record.task_specification,
            task_summary=record.task_summary,
        )
