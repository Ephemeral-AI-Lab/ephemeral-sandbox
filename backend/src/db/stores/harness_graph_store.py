"""HarnessGraph persistence store. Returns frozen DTOs."""

from __future__ import annotations

import uuid
from datetime import UTC, datetime

from db.models.harness_graph import HarnessGraphRecord
from db.stores.base import SyncStoreMixin
from task_center.attempt import (
    HarnessGraph,
    HarnessGraphFailReason,
    HarnessGraphStage,
    HarnessGraphStatus,
)


class HarnessGraphStore(SyncStoreMixin):
    """CRUD for HarnessGraph. Returns frozen HarnessGraph DTOs."""

    def insert(
        self, *, task_segment_id: str, graph_sequence_no: int
    ) -> HarnessGraph:
        with self._sf() as db:
            now = datetime.now(UTC)
            record = HarnessGraphRecord(
                id=str(uuid.uuid4()),
                task_segment_id=task_segment_id,
                graph_sequence_no=graph_sequence_no,
                stage=HarnessGraphStage.PLANNING.value,
                status=HarnessGraphStatus.RUNNING.value,
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

    def get(self, graph_id: str) -> HarnessGraph | None:
        with self._sf() as db:
            record = db.get(HarnessGraphRecord, graph_id)
            return self._to_dto(record) if record is not None else None

    def set_planner_task_id(
        self, graph_id: str, planner_task_id: str
    ) -> HarnessGraph:
        with self._sf() as db:
            record = db.get(HarnessGraphRecord, graph_id)
            if record is None:
                raise LookupError(f"HarnessGraph {graph_id!r} not found")
            record.planner_task_id = planner_task_id
            db.commit()
            db.refresh(record)
            return self._to_dto(record)

    def set_plan_contract(
        self,
        graph_id: str,
        *,
        task_specification: str,
        evaluation_criteria: list[str],
        continuation_goal: str | None,
    ) -> HarnessGraph:
        with self._sf() as db:
            record = db.get(HarnessGraphRecord, graph_id)
            if record is None:
                raise LookupError(f"HarnessGraph {graph_id!r} not found")
            record.task_specification = task_specification
            record.evaluation_criteria = list(evaluation_criteria)
            record.continuation_goal = continuation_goal
            db.commit()
            db.refresh(record)
            return self._to_dto(record)

    def set_generator_task_ids(
        self, graph_id: str, task_ids: list[str]
    ) -> HarnessGraph:
        with self._sf() as db:
            record = db.get(HarnessGraphRecord, graph_id)
            if record is None:
                raise LookupError(f"HarnessGraph {graph_id!r} not found")
            record.generator_task_ids = list(task_ids)
            db.commit()
            db.refresh(record)
            return self._to_dto(record)

    def set_evaluator_task_id(
        self, graph_id: str, evaluator_task_id: str
    ) -> HarnessGraph:
        with self._sf() as db:
            record = db.get(HarnessGraphRecord, graph_id)
            if record is None:
                raise LookupError(f"HarnessGraph {graph_id!r} not found")
            record.evaluator_task_id = evaluator_task_id
            db.commit()
            db.refresh(record)
            return self._to_dto(record)

    def set_stage(
        self, graph_id: str, stage: HarnessGraphStage
    ) -> HarnessGraph:
        with self._sf() as db:
            record = db.get(HarnessGraphRecord, graph_id)
            if record is None:
                raise LookupError(f"HarnessGraph {graph_id!r} not found")
            record.stage = stage.value
            db.commit()
            db.refresh(record)
            return self._to_dto(record)

    def close(
        self,
        graph_id: str,
        *,
        status: HarnessGraphStatus,
        fail_reason: HarnessGraphFailReason | None,
        closed_at: datetime | None = None,
    ) -> HarnessGraph:
        with self._sf() as db:
            record = db.get(HarnessGraphRecord, graph_id)
            if record is None:
                raise LookupError(f"HarnessGraph {graph_id!r} not found")
            record.stage = HarnessGraphStage.CLOSED.value
            record.status = status.value
            record.fail_reason = fail_reason.value if fail_reason is not None else None
            record.closed_at = closed_at if closed_at is not None else datetime.now(UTC)
            db.commit()
            db.refresh(record)
            return self._to_dto(record)

    def list_for_segment(self, task_segment_id: str) -> list[HarnessGraph]:
        """Ordered by graph_sequence_no ascending."""
        with self._sf() as db:
            q = (
                db.query(HarnessGraphRecord)
                .filter(HarnessGraphRecord.task_segment_id == task_segment_id)
                .order_by(HarnessGraphRecord.graph_sequence_no.asc())
            )
            return [self._to_dto(r) for r in q.all()]

    def list_for_segments(
        self, task_segment_ids: list[str]
    ) -> list[HarnessGraph]:
        """Ordered by segment id, then graph_sequence_no ascending."""
        if not task_segment_ids:
            return []
        with self._sf() as db:
            q = (
                db.query(HarnessGraphRecord)
                .filter(HarnessGraphRecord.task_segment_id.in_(task_segment_ids))
                .order_by(
                    HarnessGraphRecord.task_segment_id.asc(),
                    HarnessGraphRecord.graph_sequence_no.asc(),
                )
            )
            return [self._to_dto(r) for r in q.all()]

    def get_by_sequence(
        self, *, task_segment_id: str, graph_sequence_no: int
    ) -> HarnessGraph | None:
        with self._sf() as db:
            record = (
                db.query(HarnessGraphRecord)
                .filter(
                    HarnessGraphRecord.task_segment_id == task_segment_id,
                    HarnessGraphRecord.graph_sequence_no == graph_sequence_no,
                )
                .first()
            )
            return self._to_dto(record) if record is not None else None

    def _to_dto(self, record: HarnessGraphRecord) -> HarnessGraph:
        return HarnessGraph(
            id=record.id,
            task_segment_id=record.task_segment_id,
            graph_sequence_no=record.graph_sequence_no,
            stage=HarnessGraphStage(record.stage),
            status=HarnessGraphStatus(record.status),
            planner_task_id=record.planner_task_id,
            task_specification=record.task_specification,
            evaluation_criteria=tuple(record.evaluation_criteria or ()),
            generator_task_ids=tuple(record.generator_task_ids or ()),
            evaluator_task_id=record.evaluator_task_id,
            continuation_goal=record.continuation_goal,
            fail_reason=(
                HarnessGraphFailReason(record.fail_reason)
                if record.fail_reason is not None
                else None
            ),
            created_at=record.created_at,
            updated_at=record.updated_at,
            closed_at=record.closed_at,
        )
