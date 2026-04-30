"""TaskCenter request/run/task/harness-graph persistence store."""

from __future__ import annotations

from datetime import UTC, datetime

from db.models.task_center import (
    TaskCenterHarnessGraphRecord,
    TaskCenterRequestRecord,
    TaskCenterRunRecord,
    TaskCenterTaskRecord,
)
from db.stores.base import SyncStoreMixin


def persisted_task_id(task_center_run_id: str, task_id: str) -> str:
    return f"{task_center_run_id}:{task_id}"


def _serialize_request(record: TaskCenterRequestRecord) -> dict:
    return {
        "id": record.id,
        "cwd": record.cwd,
        "sandbox_id": record.sandbox_id,
        "request_prompt": record.request_prompt,
        "created_at": record.created_at.isoformat() if record.created_at else None,
        "updated_at": record.updated_at.isoformat() if record.updated_at else None,
    }


def _serialize_run(record: TaskCenterRunRecord) -> dict:
    return {
        "id": record.id,
        "request_id": record.request_id,
        "root_task_id": record.root_task_id,
        "status": record.status,
        "started_at": record.started_at.isoformat() if record.started_at else None,
        "finished_at": record.finished_at.isoformat() if record.finished_at else None,
    }


def _serialize_task(record: TaskCenterTaskRecord) -> dict:
    return {
        "id": record.id,
        "task_center_run_id": record.task_center_run_id,
        "role": record.role,
        "task_input": record.task_input,
        "status": record.status,
        "summaries": record.summaries or [],
        "needs": record.needs or [],
        "task_center_harness_graph_id": record.task_center_harness_graph_id,
        "fix_target_id": record.fix_target_id,
        "spawn_reason": record.spawn_reason,
        "created_at": record.created_at.isoformat() if record.created_at else None,
        "updated_at": record.updated_at.isoformat() if record.updated_at else None,
    }


def _serialize_harness_graph(record: TaskCenterHarnessGraphRecord) -> dict:
    return {
        "id": record.id,
        "task_center_run_id": record.task_center_run_id,
        "root_task_id": record.root_task_id,
        "planner_task_id": record.planner_task_id,
        "executor_task_ids": record.executor_task_ids or [],
        "dag_nodes": record.dag_nodes or [],
        "plan_shape": record.plan_shape,
        "what_to_do_next": record.what_to_do_next or "",
        "prior_graph_id": record.prior_graph_id,
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
    ) -> TaskCenterRequestRecord:
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
            return record

    def get_request(self, request_id: str) -> TaskCenterRequestRecord | None:
        with self._sf() as db:
            return db.get(TaskCenterRequestRecord, request_id)

    def list_requests(self, cwd: str | None = None, limit: int = 20) -> list[dict]:
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
    ) -> TaskCenterRunRecord:
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
            return record

    def set_run_root(self, task_center_run_id: str, root_task_id: str) -> None:
        with self._sf() as db:
            record = db.get(TaskCenterRunRecord, task_center_run_id)
            if record is None:
                return
            record.root_task_id = root_task_id
            db.commit()

    def finish_run(self, task_center_run_id: str, status: str) -> None:
        with self._sf() as db:
            record = db.get(TaskCenterRunRecord, task_center_run_id)
            if record is None:
                return
            record.status = status
            record.finished_at = datetime.now(UTC)
            db.commit()

    def get_run(self, task_center_run_id: str) -> TaskCenterRunRecord | None:
        with self._sf() as db:
            return db.get(TaskCenterRunRecord, task_center_run_id)

    def list_runs_for_request(self, request_id: str, limit: int = 50) -> list[dict]:
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
        task_input: str,
        status: str,
        summaries: list[dict],
        needs: list[str],
        task_center_harness_graph_id: str | None,
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
                    task_input=task_input,
                    status=status,
                    summaries=summaries,
                    needs=needs,
                    task_center_harness_graph_id=task_center_harness_graph_id,
                    fix_target_id=fix_target_id,
                    spawn_reason=spawn_reason,
                    created_at=now,
                    updated_at=now,
                )
                db.add(record)
            else:
                record.role = role
                record.task_input = task_input
                record.status = status
                record.summaries = summaries
                record.needs = needs
                record.task_center_harness_graph_id = task_center_harness_graph_id
                record.fix_target_id = fix_target_id
                record.spawn_reason = spawn_reason
                record.updated_at = now
            db.commit()

    def upsert_harness_graph(
        self,
        *,
        graph_id: str,
        task_center_run_id: str,
        root_task_id: str,
        planner_task_id: str,
        executor_task_ids: list[str],
        dag_nodes: list[str] | None = None,
        plan_shape: str | None = None,
        what_to_do_next: str = "",
        prior_graph_id: str | None = None,
    ) -> None:
        with self._sf() as db:
            now = datetime.now(UTC)
            record = db.get(TaskCenterHarnessGraphRecord, graph_id)
            if record is None:
                record = TaskCenterHarnessGraphRecord(
                    id=graph_id,
                    task_center_run_id=task_center_run_id,
                    root_task_id=root_task_id,
                    planner_task_id=planner_task_id,
                    executor_task_ids=executor_task_ids,
                    dag_nodes=list(dag_nodes) if dag_nodes is not None else [],
                    plan_shape=plan_shape,
                    what_to_do_next=what_to_do_next or "",
                    prior_graph_id=prior_graph_id,
                    created_at=now,
                    updated_at=now,
                )
                db.add(record)
            else:
                record.root_task_id = root_task_id
                record.planner_task_id = planner_task_id
                record.executor_task_ids = executor_task_ids
                if dag_nodes is not None:
                    record.dag_nodes = list(dag_nodes)
                record.plan_shape = plan_shape
                record.what_to_do_next = what_to_do_next or ""
                record.prior_graph_id = prior_graph_id
                record.updated_at = now
            db.commit()

    def list_tasks_for_run(self, task_center_run_id: str) -> list[dict]:
        with self._sf() as db:
            q = (
                db.query(TaskCenterTaskRecord)
                .filter(TaskCenterTaskRecord.task_center_run_id == task_center_run_id)
                .order_by(TaskCenterTaskRecord.created_at.asc())
            )
            return [_serialize_task(record) for record in q.all()]

    def list_harness_graphs_for_run(self, task_center_run_id: str) -> list[dict]:
        with self._sf() as db:
            q = (
                db.query(TaskCenterHarnessGraphRecord)
                .filter(
                    TaskCenterHarnessGraphRecord.task_center_run_id
                    == task_center_run_id
                )
                .order_by(TaskCenterHarnessGraphRecord.created_at.asc())
            )
            return [_serialize_harness_graph(record) for record in q.all()]
