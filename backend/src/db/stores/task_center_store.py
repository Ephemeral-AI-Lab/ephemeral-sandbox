"""TaskCenter request/run/task persistence store.

Harness-graph persistence has moved to ``db.stores.attempt_store``
and is owned by the new three-axis (request / segment / graph) schema.
"""

from __future__ import annotations

from datetime import UTC, datetime
from typing import Any

from db.models.task_center import (
    TaskCenterRequestRecord,
    TaskCenterRunRecord,
    TaskCenterTaskRecord,
)
from db.stores.base import SyncStoreMixin


SerializedRow = dict[str, Any]


def _serialize_request(record: TaskCenterRequestRecord) -> SerializedRow:
    return {
        "id": record.id,
        "cwd": record.cwd,
        "sandbox_id": record.sandbox_id,
        "request_prompt": record.request_prompt,
        "created_at": record.created_at.isoformat() if record.created_at else None,
        "updated_at": record.updated_at.isoformat() if record.updated_at else None,
    }


def _serialize_run(record: TaskCenterRunRecord) -> SerializedRow:
    return {
        "id": record.id,
        "request_id": record.request_id,
        "status": record.status,
        "started_at": record.started_at.isoformat() if record.started_at else None,
        "finished_at": record.finished_at.isoformat() if record.finished_at else None,
    }


def _serialize_task(record: TaskCenterTaskRecord) -> SerializedRow:
    return {
        "id": record.id,
        "task_center_run_id": record.task_center_run_id,
        "role": record.role,
        "agent_name": record.agent_name,
        "rendered_prompt": record.rendered_prompt,
        "status": record.status,
        "summaries": record.summaries or [],
        "needs": record.needs or [],
        "task_center_attempt_id": record.task_center_attempt_id,
        "context_packet_id": record.context_packet_id,
        "fix_target_id": record.fix_target_id,
        "spawn_reason": record.spawn_reason,
        "created_at": record.created_at.isoformat() if record.created_at else None,
        "updated_at": record.updated_at.isoformat() if record.updated_at else None,
    }


class TaskCenterStore(SyncStoreMixin):
    """CRUD operations for TaskCenter persistence."""

    def create_request(
        self,
        *,
        request_id: str,
        cwd: str,
        sandbox_id: str | None,
        request_prompt: str,
    ) -> SerializedRow:
        with self._sf() as db:
            now = datetime.now(UTC)
            record = TaskCenterRequestRecord(
                id=request_id,
                cwd=cwd,
                sandbox_id=sandbox_id,
                request_prompt=request_prompt,
                created_at=now,
                updated_at=now,
            )
            db.add(record)
            db.commit()
            db.refresh(record)
            return _serialize_request(record)

    def get_request(self, request_id: str) -> SerializedRow | None:
        with self._sf() as db:
            record = db.get(TaskCenterRequestRecord, request_id)
            return _serialize_request(record) if record is not None else None

    def list_requests(
        self, cwd: str | None = None, limit: int = 20
    ) -> list[SerializedRow]:
        with self._sf() as db:
            q = db.query(TaskCenterRequestRecord)
            if cwd:
                q = q.filter(TaskCenterRequestRecord.cwd == cwd)
            q = q.order_by(TaskCenterRequestRecord.created_at.desc()).limit(limit)
            return [_serialize_request(record) for record in q.all()]

    def create_run(
        self,
        *,
        task_center_run_id: str,
        request_id: str,
    ) -> SerializedRow:
        with self._sf() as db:
            record = TaskCenterRunRecord(
                id=task_center_run_id,
                request_id=request_id,
                status="running",
                started_at=datetime.now(UTC),
            )
            db.add(record)
            db.commit()
            db.refresh(record)
            return _serialize_run(record)

    def finish_run(self, task_center_run_id: str, status: str) -> None:
        with self._sf() as db:
            record = db.get(TaskCenterRunRecord, task_center_run_id)
            if record is None:
                return
            record.status = status
            record.finished_at = datetime.now(UTC)
            db.commit()

    def get_run(self, task_center_run_id: str) -> SerializedRow | None:
        with self._sf() as db:
            record = db.get(TaskCenterRunRecord, task_center_run_id)
            return _serialize_run(record) if record is not None else None

    def list_runs_for_request(
        self, request_id: str, limit: int = 50
    ) -> list[SerializedRow]:
        with self._sf() as db:
            q = (
                db.query(TaskCenterRunRecord)
                .filter(TaskCenterRunRecord.request_id == request_id)
                .order_by(TaskCenterRunRecord.started_at.desc())
                .limit(limit)
            )
            return [_serialize_run(record) for record in q.all()]

    def upsert_task(
        self,
        *,
        task_id: str,
        task_center_run_id: str,
        role: str,
        rendered_prompt: str,
        status: str,
        summaries: list[SerializedRow],
        needs: list[str],
        task_center_attempt_id: str | None,
        agent_name: str | None = None,
        context_packet_id: str | None = None,
        fix_target_id: str | None = None,
        spawn_reason: str | None = None,
    ) -> None:
        with self._sf() as db:
            now = datetime.now(UTC)
            record = db.get(TaskCenterTaskRecord, task_id)
            if record is None:
                record = TaskCenterTaskRecord(
                    id=task_id,
                    task_center_run_id=task_center_run_id,
                    role=role,
                    agent_name=agent_name,
                    rendered_prompt=rendered_prompt,
                    status=status,
                    summaries=summaries,
                    needs=needs,
                    task_center_attempt_id=task_center_attempt_id,
                    context_packet_id=context_packet_id,
                    fix_target_id=fix_target_id,
                    spawn_reason=spawn_reason,
                    created_at=now,
                    updated_at=now,
                )
                db.add(record)
            else:
                record.role = role
                record.agent_name = agent_name
                record.rendered_prompt = rendered_prompt
                record.status = status
                record.summaries = summaries
                record.needs = needs
                record.task_center_attempt_id = task_center_attempt_id
                record.context_packet_id = context_packet_id
                record.fix_target_id = fix_target_id
                record.spawn_reason = spawn_reason
                record.updated_at = now
            db.commit()

    def get_task(self, task_id: str) -> SerializedRow | None:
        with self._sf() as db:
            record = db.get(TaskCenterTaskRecord, task_id)
            return _serialize_task(record) if record is not None else None

    def list_tasks_for_run(self, task_center_run_id: str) -> list[SerializedRow]:
        with self._sf() as db:
            q = (
                db.query(TaskCenterTaskRecord)
                .filter(TaskCenterTaskRecord.task_center_run_id == task_center_run_id)
                .order_by(TaskCenterTaskRecord.created_at.asc())
            )
            return [_serialize_task(record) for record in q.all()]

    def list_tasks_for_attempt(
        self, attempt_id: str
    ) -> list[SerializedRow]:
        with self._sf() as db:
            q = (
                db.query(TaskCenterTaskRecord)
                .filter(
                    TaskCenterTaskRecord.task_center_attempt_id
                    == attempt_id
                )
                .order_by(TaskCenterTaskRecord.created_at.asc())
            )
            return [_serialize_task(record) for record in q.all()]

    def list_generator_tasks_for_attempt(
        self, attempt_id: str
    ) -> list[SerializedRow]:
        with self._sf() as db:
            q = (
                db.query(TaskCenterTaskRecord)
                .filter(
                    TaskCenterTaskRecord.task_center_attempt_id
                    == attempt_id,
                    TaskCenterTaskRecord.role == "generator",
                )
                .order_by(TaskCenterTaskRecord.created_at.asc())
            )
            return [_serialize_task(record) for record in q.all()]

    def get_evaluator_pass_summary(self, attempt_id: str) -> str:
        """Return the evaluator's latest success-summary text for *graph*.

        Used by the segment manager to denormalize the closing evaluator's
        summary onto the segment row at success-close. Returns an empty
        string when no evaluator task is found for the graph or the
        evaluator never recorded a summary (defensive fallback — the caller
        treats empty strings as ``null``-equivalent).
        """
        with self._sf() as db:
            record = (
                db.query(TaskCenterTaskRecord)
                .filter(
                    TaskCenterTaskRecord.task_center_attempt_id
                    == attempt_id,
                    TaskCenterTaskRecord.role == "evaluator",
                )
                .order_by(TaskCenterTaskRecord.created_at.desc())
                .first()
            )
            if record is None:
                return ""
            summaries = record.summaries or []
            if not summaries:
                return ""
            latest = summaries[-1]
            return str(latest.get("summary") or "") if isinstance(latest, dict) else ""

    def set_task_status(
        self,
        task_id: str,
        *,
        status: str,
        summary: SerializedRow | None = None,
    ) -> SerializedRow:
        with self._sf() as db:
            record = db.get(TaskCenterTaskRecord, task_id)
            if record is None:
                raise LookupError(f"TaskCenterTask {task_id!r} not found")
            record.status = status
            if summary is not None:
                record.summaries = [*(record.summaries or []), summary]
            record.updated_at = datetime.now(UTC)
            db.commit()
            db.refresh(record)
            return _serialize_task(record)

    def set_task_context_packet_id(
        self,
        task_id: str,
        *,
        context_packet_id: str | None,
    ) -> SerializedRow:
        with self._sf() as db:
            record = db.get(TaskCenterTaskRecord, task_id)
            if record is None:
                raise LookupError(f"TaskCenterTask {task_id!r} not found")
            record.context_packet_id = context_packet_id
            record.updated_at = datetime.now(UTC)
            db.commit()
            db.refresh(record)
            return _serialize_task(record)

    def set_task_status_if_current(
        self,
        task_id: str,
        *,
        expected_status: str,
        status: str,
        summary: SerializedRow | None = None,
    ) -> SerializedRow | None:
        """Compare-and-set task status. Returns the new row, or ``None`` on mismatch.

        The CAS miss is the idempotency primitive for parent-task transitions
        in the complex-task-handoff lifecycle.
        """
        with self._sf() as db:
            record = db.get(TaskCenterTaskRecord, task_id)
            if record is None:
                raise LookupError(f"TaskCenterTask {task_id!r} not found")
            if record.status != expected_status:
                return None
            record.status = status
            if summary is not None:
                record.summaries = [*(record.summaries or []), summary]
            record.updated_at = datetime.now(UTC)
            db.commit()
            db.refresh(record)
            return _serialize_task(record)
