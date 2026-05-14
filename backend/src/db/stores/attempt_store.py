"""Attempt persistence store. Returns frozen DTOs."""

from __future__ import annotations

import uuid
from datetime import UTC, datetime

from db.models.attempt import AttemptRecord
from db.stores.base import SyncStoreMixin
from task_center.domain import (
    Attempt,
    AttemptFailReason,
    AttemptStage,
    AttemptStatus,
)


class AttemptStore(SyncStoreMixin):
    """CRUD for Attempt. Returns frozen Attempt DTOs."""

    def insert(
        self, *, episode_id: str, attempt_sequence_no: int
    ) -> Attempt:
        with self._sf() as db:
            now = datetime.now(UTC)
            record = AttemptRecord(
                id=str(uuid.uuid4()),
                episode_id=episode_id,
                attempt_sequence_no=attempt_sequence_no,
                stage=AttemptStage.PLAN.value,
                status=AttemptStatus.RUNNING.value,
                planner_task_id=None,
                task_specification=None,
                evaluation_criteria=[],
                generator_task_ids=[],
                evaluator_task_id=None,
                continuation_goal=None,
                fail_reason=None,
                created_at=now,
                updated_at=now,
            )
            db.add(record)
            db.commit()
            db.refresh(record)
            return self._to_dto(record)

    def get(self, attempt_id: str) -> Attempt | None:
        with self._sf() as db:
            record = db.get(AttemptRecord, attempt_id)
            return self._to_dto(record) if record is not None else None

    def set_planner_task_id(
        self, attempt_id: str, planner_task_id: str
    ) -> Attempt:
        with self._sf() as db:
            record = db.get(AttemptRecord, attempt_id)
            if record is None:
                raise LookupError(f"Attempt {attempt_id!r} not found")
            record.planner_task_id = planner_task_id
            db.commit()
            db.refresh(record)
            return self._to_dto(record)

    def set_plan_contract(
        self,
        attempt_id: str,
        *,
        task_specification: str,
        evaluation_criteria: list[str],
        continuation_goal: str | None,
    ) -> Attempt:
        with self._sf() as db:
            record = db.get(AttemptRecord, attempt_id)
            if record is None:
                raise LookupError(f"Attempt {attempt_id!r} not found")
            record.task_specification = task_specification
            record.evaluation_criteria = list(evaluation_criteria)
            record.continuation_goal = continuation_goal
            db.commit()
            db.refresh(record)
            return self._to_dto(record)

    def set_generator_task_ids(
        self, attempt_id: str, task_ids: list[str]
    ) -> Attempt:
        with self._sf() as db:
            record = db.get(AttemptRecord, attempt_id)
            if record is None:
                raise LookupError(f"Attempt {attempt_id!r} not found")
            record.generator_task_ids = list(task_ids)
            db.commit()
            db.refresh(record)
            return self._to_dto(record)

    def set_evaluator_task_id(
        self, attempt_id: str, evaluator_task_id: str
    ) -> Attempt:
        with self._sf() as db:
            record = db.get(AttemptRecord, attempt_id)
            if record is None:
                raise LookupError(f"Attempt {attempt_id!r} not found")
            record.evaluator_task_id = evaluator_task_id
            db.commit()
            db.refresh(record)
            return self._to_dto(record)

    def set_stage(
        self, attempt_id: str, stage: AttemptStage
    ) -> Attempt:
        with self._sf() as db:
            record = db.get(AttemptRecord, attempt_id)
            if record is None:
                raise LookupError(f"Attempt {attempt_id!r} not found")
            record.stage = stage.value
            db.commit()
            db.refresh(record)
            return self._to_dto(record)

    def close(
        self,
        attempt_id: str,
        *,
        status: AttemptStatus,
        fail_reason: AttemptFailReason | None,
        closed_at: datetime | None = None,
    ) -> Attempt:
        with self._sf() as db:
            record = db.get(AttemptRecord, attempt_id)
            if record is None:
                raise LookupError(f"Attempt {attempt_id!r} not found")
            record.stage = AttemptStage.CLOSED.value
            record.status = status.value
            record.fail_reason = fail_reason.value if fail_reason is not None else None
            record.closed_at = closed_at if closed_at is not None else datetime.now(UTC)
            db.commit()
            db.refresh(record)
            return self._to_dto(record)

    def list_for_episode(self, episode_id: str) -> list[Attempt]:
        """Ordered by attempt_sequence_no ascending."""
        with self._sf() as db:
            q = (
                db.query(AttemptRecord)
                .filter(AttemptRecord.episode_id == episode_id)
                .order_by(AttemptRecord.attempt_sequence_no.asc())
            )
            return [self._to_dto(r) for r in q.all()]

    def get_by_sequence(
        self, *, episode_id: str, attempt_sequence_no: int
    ) -> Attempt | None:
        with self._sf() as db:
            record = (
                db.query(AttemptRecord)
                .filter(
                    AttemptRecord.episode_id == episode_id,
                    AttemptRecord.attempt_sequence_no == attempt_sequence_no,
                )
                .first()
            )
            return self._to_dto(record) if record is not None else None

    def _to_dto(self, record: AttemptRecord) -> Attempt:
        return Attempt(
            id=record.id,
            episode_id=record.episode_id,
            attempt_sequence_no=record.attempt_sequence_no,
            stage=AttemptStage(record.stage),
            status=AttemptStatus(record.status),
            planner_task_id=record.planner_task_id,
            task_specification=record.task_specification,
            evaluation_criteria=tuple(record.evaluation_criteria or ()),
            generator_task_ids=tuple(record.generator_task_ids or ()),
            evaluator_task_id=record.evaluator_task_id,
            continuation_goal=record.continuation_goal,
            fail_reason=(
                AttemptFailReason(record.fail_reason)
                if record.fail_reason is not None
                else None
            ),
            created_at=record.created_at,
            updated_at=record.updated_at,
            closed_at=record.closed_at,
        )
