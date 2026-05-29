"""Workflow persistence store. Returns frozen DTOs."""

from __future__ import annotations

import uuid
from datetime import UTC, datetime
from typing import Any

from db.models.workflow import WorkflowRecord
from db.stores.base import SyncStoreMixin
from task_center.workflow.state import (
    WorkflowOrigin,
    WorkflowOriginKind,
    Workflow,
    WorkflowStatus,
)


class WorkflowStore(SyncStoreMixin):
    """CRUD for Workflow. Returns frozen Workflow DTOs."""

    def insert(
        self,
        *,
        task_center_run_id: str,
        origin: WorkflowOrigin | None = None,
        requested_by_task_id: str | None = None,
        goal: str,
    ) -> Workflow:
        origin = _resolve_origin(
            task_center_run_id=task_center_run_id,
            origin=origin,
            requested_by_task_id=requested_by_task_id,
        )
        with self._sf() as db:
            now = datetime.now(UTC)
            record = WorkflowRecord(
                id=str(uuid.uuid4()),
                task_center_run_id=task_center_run_id,
                origin_kind=origin.kind.value,
                requested_by_task_id=origin.task_id,
                goal=goal,
                status=WorkflowStatus.OPEN.value,
                iteration_ids=[],
                final_outcome=None,
                created_at=now,
                updated_at=now,
            )
            db.add(record)
            db.commit()
            db.refresh(record)
            return self._to_dto(record)

    def get(self, workflow_id: str) -> Workflow | None:
        with self._sf() as db:
            record = db.get(WorkflowRecord, workflow_id)
            return self._to_dto(record) if record is not None else None

    def append_iteration_id(
        self, workflow_id: str, iteration_id: str
    ) -> Workflow:
        with self._sf() as db:
            record = db.get(WorkflowRecord, workflow_id)
            if record is None:
                raise LookupError(f"Workflow {workflow_id!r} not found")
            ids = list(record.iteration_ids or [])
            ids.append(iteration_id)
            record.iteration_ids = ids
            db.commit()
            db.refresh(record)
            return self._to_dto(record)

    def set_status(
        self,
        workflow_id: str,
        *,
        status: WorkflowStatus,
        final_outcome: dict[str, Any] | None,
        closed_at: datetime | None = None,
    ) -> Workflow:
        with self._sf() as db:
            record = db.get(WorkflowRecord, workflow_id)
            if record is None:
                raise LookupError(f"Workflow {workflow_id!r} not found")
            record.status = status.value
            record.final_outcome = final_outcome
            if closed_at is not None:
                record.closed_at = closed_at
            db.commit()
            db.refresh(record)
            return self._to_dto(record)

    def list_for_parent_task(self, parent_task_id: str) -> list[Workflow]:
        with self._sf() as db:
            q = (
                db.query(WorkflowRecord)
                .filter(
                    WorkflowRecord.requested_by_task_id
                    == parent_task_id
                )
                .order_by(WorkflowRecord.created_at.asc())
            )
            return [self._to_dto(r) for r in q.all()]

    def list_for_run(
        self, task_center_run_id: str
    ) -> list[Workflow]:
        with self._sf() as db:
            q = (
                db.query(WorkflowRecord)
                .filter(
                    WorkflowRecord.task_center_run_id
                    == task_center_run_id
                )
                .order_by(WorkflowRecord.created_at.asc())
            )
            return [self._to_dto(r) for r in q.all()]

    def _to_dto(self, record: WorkflowRecord) -> Workflow:
        return Workflow(
            id=record.id,
            task_center_run_id=record.task_center_run_id,
            origin_kind=WorkflowOriginKind(record.origin_kind or WorkflowOriginKind.TASK.value),
            requested_by_task_id=record.requested_by_task_id,
            goal=record.goal,
            status=WorkflowStatus(record.status),
            iteration_ids=tuple(record.iteration_ids or ()),
            final_outcome=record.final_outcome,
            created_at=record.created_at,
            updated_at=record.updated_at,
            closed_at=record.closed_at,
        )


def _resolve_origin(
    *,
    task_center_run_id: str,
    origin: WorkflowOrigin | None,
    requested_by_task_id: str | None,
) -> WorkflowOrigin:
    if origin is not None:
        return origin
    if requested_by_task_id is not None:
        return WorkflowOrigin.task(task_id=requested_by_task_id)
    return WorkflowOrigin.entry(task_center_run_id=task_center_run_id)
