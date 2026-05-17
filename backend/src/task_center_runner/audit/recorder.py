"""AuditRecorder — directory writer + ORM commit listeners.

Wires five SQLAlchemy ``after_insert``/``after_update`` listeners (one per
``GoalRecord``/``IterationRecord``/``AttemptRecord``/``TaskCenterTaskRecord``
plus a fifth on ``AgentRunRecord`` for ``agent_run_id`` -> ``task_id``
mapping). Task stream events append conversation-message rows to
``message.jsonl``. Lifecycle rows are mirrored as latest-state ``*.json``
snapshots under a hierarchical run directory, while sandbox subsystem monitor
events are mirrored into ``sandbox_events.jsonl``.
"""

from __future__ import annotations

import time
from collections.abc import Callable
from dataclasses import asdict, dataclass
from datetime import datetime
from pathlib import Path
from typing import Any

from sqlalchemy import event

from audit.jsonl import append_jsonl_event
from task_center_runner.audit.bus import AuditEventBus
from task_center_runner.audit.events import Event as AuditEvent
from task_center_runner.audit.io import atomic_write_json
from task_center_runner.audit.metrics import MetricsAggregator
from db.models.agent_run import AgentRunRecord
from db.models.attempt import AttemptRecord
from db.models.iteration import IterationRecord
from db.models.goal import GoalRecord
from db.models.task_center import TaskCenterTaskRecord
from message.agent_message_recorder import (
    AgentMessageJsonlRecorder,
    clear_recorder_for_agent_run,
    register_recorder_for_agent_run,
)


PRIMARY_ROLES: frozenset[str] = frozenset(
    {"entry_executor", "planner", "executor", "verifier", "evaluator"}
)

# Roles which earn an ``NN_<role>_<task_id>`` directory under the parent
# attempt — superset of the primary message-recorder allowlist (we still
# want the ``task.json`` snapshot for ``generator`` rows).
_ATTEMPT_CHILD_ROLES: frozenset[str] = frozenset(
    {"planner", "executor", "verifier", "evaluator", "generator"}
)


def _isoformat(value: datetime | None) -> str | None:
    return value.isoformat() if value is not None else None


def _serialize_goal(record: GoalRecord) -> dict[str, Any]:
    return {
        "id": record.id,
        "task_center_run_id": record.task_center_run_id,
        "requested_by_task_id": record.requested_by_task_id,
        "goal": record.goal,
        "status": record.status,
        "iteration_ids": list(record.iteration_ids or []),
        "final_outcome": record.final_outcome,
        "created_at": _isoformat(record.created_at),
        "updated_at": _isoformat(record.updated_at),
        "closed_at": _isoformat(record.closed_at),
    }


def _serialize_iteration(record: IterationRecord) -> dict[str, Any]:
    return {
        "id": record.id,
        "goal_id": record.goal_id,
        "sequence_no": record.sequence_no,
        "creation_reason": record.creation_reason,
        "goal": record.goal,
        "attempt_budget": record.attempt_budget,
        "status": record.status,
        "attempt_ids": list(record.attempt_ids or []),
        "continuation_goal": record.continuation_goal,
        "task_specification": record.task_specification,
        "task_summary": record.task_summary,
        "created_at": _isoformat(record.created_at),
        "updated_at": _isoformat(record.updated_at),
        "closed_at": _isoformat(record.closed_at),
    }


def _serialize_attempt(record: AttemptRecord) -> dict[str, Any]:
    return {
        "id": record.id,
        "iteration_id": record.iteration_id,
        "attempt_sequence_no": record.attempt_sequence_no,
        "stage": record.stage,
        "status": record.status,
        "planner_task_id": record.planner_task_id,
        "task_specification": record.task_specification,
        "evaluation_criteria": list(record.evaluation_criteria or []),
        "generator_task_ids": list(record.generator_task_ids or []),
        "evaluator_task_id": record.evaluator_task_id,
        "continuation_goal": record.continuation_goal,
        "fail_reason": record.fail_reason,
        "created_at": _isoformat(record.created_at),
        "updated_at": _isoformat(record.updated_at),
        "closed_at": _isoformat(record.closed_at),
    }


def _serialize_task(record: TaskCenterTaskRecord) -> dict[str, Any]:
    return {
        "id": record.id,
        "task_center_run_id": record.task_center_run_id,
        "role": record.role,
        "agent_name": record.agent_name,
        "rendered_prompt": record.rendered_prompt,
        "status": record.status,
        "summaries": list(record.summaries or []),
        "needs": list(record.needs or []),
        "task_center_attempt_id": record.task_center_attempt_id,
        "context_packet_id": record.context_packet_id,
        "fix_target_id": record.fix_target_id,
        "spawn_reason": record.spawn_reason,
        "created_at": _isoformat(record.created_at),
        "updated_at": _isoformat(record.updated_at),
    }


_atomic_write_json = atomic_write_json


@dataclass(slots=True)
class _ListenerHandle:
    """Bookkeeping for a single ``sqlalchemy.event.listens_for`` registration."""

    target: Any
    identifier: str
    fn: Callable[..., None]


class AuditRecorder:
    """Mirror SQLAlchemy commits into a hierarchical on-disk audit tree."""

    def __init__(
        self,
        run_dir: Path,
        *,
        task_center_run_id: str,
        bus: AuditEventBus | None = None,
        primary_roles: frozenset[str] = PRIMARY_ROLES,
        scenario_name: str = "",
        instance_id: str = "",
        sandbox_id: str = "",
    ) -> None:
        self._run_dir = Path(run_dir)
        self._task_center_run_id = task_center_run_id
        self._bus = bus
        self._primary_roles = frozenset(primary_roles)
        self._scenario_name = scenario_name
        self._instance_id = instance_id
        self._sandbox_id = sandbox_id

        self._goal_dir: dict[str, Path] = {}
        self._iteration_dir: dict[str, Path] = {}
        self._attempt_dir: dict[str, Path] = {}
        self._task_dir: dict[str, Path] = {}
        self._task_recorder: dict[str, AgentMessageJsonlRecorder] = {}
        self._agent_run_to_task: dict[str, str] = {}

        self._goal_seq_counter: int = 0
        self._iteration_seq_counter: dict[str, int] = {}
        self._attempt_seq_counter: dict[str, int] = {}
        self._role_seq_counter: dict[str, int] = {}

        self._listeners: list[_ListenerHandle] = []
        self._metrics = MetricsAggregator()
        self._metrics_unsub: Callable[[], None] | None = None
        self._sandbox_events_unsub: Callable[[], None] | None = None

        self._started_ts: float | None = None
        self._finished_ts: float | None = None
        self._status: str = "pending"

    # ------------------------------------------------------------------
    # Public API
    # ------------------------------------------------------------------

    @property
    def run_dir(self) -> Path:
        return self._run_dir

    @property
    def metrics(self) -> MetricsAggregator:
        return self._metrics

    def message_recorder_for_task(
        self, task_id: str
    ) -> AgentMessageJsonlRecorder | None:
        return self._task_recorder.get(task_id)

    def bind_task_center_run_id(self, task_center_run_id: str) -> None:
        """Bind the run id once known. Triggers a refresh of run.json."""
        self._task_center_run_id = task_center_run_id
        if self._started_ts is not None:
            self._write_run_json()

    def message_recorder_for_agent_run(
        self, agent_run_id: str
    ) -> AgentMessageJsonlRecorder | None:
        task_id = self._agent_run_to_task.get(agent_run_id)
        if task_id is None:
            return None
        return self._task_recorder.get(task_id)

    def start(self) -> None:
        """Register the 5 SQLAlchemy listeners and write run.json."""
        self._run_dir.mkdir(parents=True, exist_ok=True)
        self._started_ts = time.time()
        self._status = "running"

        self._register(
            GoalRecord,
            "after_insert",
            lambda mapper, connection, target: self._handle_goal(target),
        )
        self._register(
            GoalRecord,
            "after_update",
            lambda mapper, connection, target: self._handle_goal(target),
        )
        self._register(
            IterationRecord,
            "after_insert",
            lambda mapper, connection, target: self._handle_iteration(target),
        )
        self._register(
            IterationRecord,
            "after_update",
            lambda mapper, connection, target: self._handle_iteration(target),
        )
        self._register(
            AttemptRecord,
            "after_insert",
            lambda mapper, connection, target: self._handle_attempt(target),
        )
        self._register(
            AttemptRecord,
            "after_update",
            lambda mapper, connection, target: self._handle_attempt(target),
        )
        self._register(
            TaskCenterTaskRecord,
            "after_insert",
            lambda mapper, connection, target: self._handle_task(target),
        )
        self._register(
            TaskCenterTaskRecord,
            "after_update",
            lambda mapper, connection, target: self._handle_task(target),
        )
        self._register(
            AgentRunRecord,
            "after_insert",
            lambda mapper, connection, target: self._handle_agent_run(target),
        )
        self._register(
            AgentRunRecord,
            "after_update",
            lambda mapper, connection, target: self._handle_agent_run(target),
        )

        if self._bus is not None:
            self._metrics_unsub = self._bus.subscribe(self._metrics.observe)
            self._sandbox_events_unsub = self._bus.subscribe(
                self._record_sandbox_event
            )

        self._write_run_json()

    def dispose(self) -> None:
        """Unregister listeners, flush message recorders, write final files."""
        for handle in self._listeners:
            try:
                event.remove(handle.target, handle.identifier, handle.fn)
            except Exception:  # noqa: BLE001 — listener may have been GC'd already
                pass
        self._listeners.clear()

        if self._metrics_unsub is not None:
            try:
                self._metrics_unsub()
            except Exception:  # noqa: BLE001
                pass
            self._metrics_unsub = None

        if self._sandbox_events_unsub is not None:
            try:
                self._sandbox_events_unsub()
            except Exception:  # noqa: BLE001
                pass
            self._sandbox_events_unsub = None

        for recorder in self._task_recorder.values():
            try:
                recorder.flush()
            except Exception:  # noqa: BLE001
                pass

        for agent_run_id in list(self._agent_run_to_task):
            clear_recorder_for_agent_run(agent_run_id)

        self._finished_ts = time.time()
        if self._status == "running":
            self._status = "finished"
        self._write_run_json()
        _atomic_write_json(self._run_dir / "metrics.json", self._metrics.snapshot())

    # ------------------------------------------------------------------
    # Listener registration
    # ------------------------------------------------------------------

    def _register(
        self,
        target: Any,
        identifier: str,
        fn: Callable[..., None],
    ) -> None:
        event.listen(target, identifier, fn)
        self._listeners.append(_ListenerHandle(target, identifier, fn))

    # ------------------------------------------------------------------
    # Handlers
    # ------------------------------------------------------------------

    def _handle_goal(self, target: GoalRecord) -> None:
        if (
            self._task_center_run_id
            and target.task_center_run_id != self._task_center_run_id
        ):
            return
        goal_dir = self._ensure_goal_dir(target.id)
        _atomic_write_json(goal_dir / "goal.json", _serialize_goal(target))

    def _handle_iteration(self, target: IterationRecord) -> None:
        goal_dir = self._goal_dir.get(target.goal_id)
        if goal_dir is None:
            return
        iteration_dir = self._ensure_iteration_dir(
            target.goal_id, target.id, goal_dir
        )
        _atomic_write_json(iteration_dir / "iteration.json", _serialize_iteration(target))

    def _handle_attempt(self, target: AttemptRecord) -> None:
        iteration_dir = self._iteration_dir.get(target.iteration_id)
        if iteration_dir is None:
            return
        attempt_dir = self._ensure_attempt_dir(
            target.iteration_id, target.id, iteration_dir
        )
        _atomic_write_json(attempt_dir / "attempt.json", _serialize_attempt(target))

    def _handle_task(self, target: TaskCenterTaskRecord) -> None:
        if (
            self._task_center_run_id
            and target.task_center_run_id != self._task_center_run_id
        ):
            return
        task_dir = self._task_dir.get(target.id)
        if task_dir is None:
            task_dir = self._resolve_task_dir(target)
            if task_dir is None:
                return
            self._task_dir[target.id] = task_dir
            task_dir.mkdir(parents=True, exist_ok=True)
            display_role = self._display_role(target)
            primary = (
                display_role in self._primary_roles
                or self._is_entry_executor(target)
            )
            if primary:
                recorder = AgentMessageJsonlRecorder(
                    task_dir / "message.jsonl",
                    base_event={
                        "task_id": target.id,
                        "task_center_run_id": self._task_center_run_id,
                    },
                )
                self._task_recorder[target.id] = recorder
                for agent_run_id, task_id in self._agent_run_to_task.items():
                    if task_id == target.id:
                        register_recorder_for_agent_run(agent_run_id, recorder)
        _atomic_write_json(task_dir / "task.json", _serialize_task(target))

    def _handle_agent_run(self, target: AgentRunRecord) -> None:
        self._agent_run_to_task[target.id] = target.task_id
        recorder = self._task_recorder.get(target.task_id)
        if recorder is not None:
            register_recorder_for_agent_run(target.id, recorder)

    def _record_sandbox_event(self, audit_event: AuditEvent) -> None:
        if not audit_event.type.value.startswith("sandbox_"):
            return
        append_jsonl_event(
            self._run_dir / "sandbox_events.jsonl",
            {
                "ts": audit_event.ts.isoformat(),
                "event_type": audit_event.type.value,
                "node": asdict(audit_event.node),
                "payload": audit_event.payload,
                "correlation_id": audit_event.correlation_id,
            },
        )

    # ------------------------------------------------------------------
    # Path resolution + numeric prefixes
    # ------------------------------------------------------------------

    def _ensure_goal_dir(self, goal_id: str) -> Path:
        cached = self._goal_dir.get(goal_id)
        if cached is not None:
            return cached
        self._goal_seq_counter += 1
        seq = self._goal_seq_counter
        path = self._run_dir / f"goal_{seq:02d}_{goal_id}"
        path.mkdir(parents=True, exist_ok=True)
        self._goal_dir[goal_id] = path
        return path

    def _ensure_iteration_dir(
        self, goal_id: str, iteration_id: str, goal_dir: Path
    ) -> Path:
        cached = self._iteration_dir.get(iteration_id)
        if cached is not None:
            return cached
        seq = self._iteration_seq_counter.get(goal_id, 0) + 1
        self._iteration_seq_counter[goal_id] = seq
        path = goal_dir / f"iteration_{seq:02d}_{iteration_id}"
        path.mkdir(parents=True, exist_ok=True)
        self._iteration_dir[iteration_id] = path
        return path

    def _ensure_attempt_dir(
        self, iteration_id: str, attempt_id: str, iteration_dir: Path
    ) -> Path:
        cached = self._attempt_dir.get(attempt_id)
        if cached is not None:
            return cached
        seq = self._attempt_seq_counter.get(iteration_id, 0) + 1
        self._attempt_seq_counter[iteration_id] = seq
        path = iteration_dir / f"attempt_{seq:02d}_{attempt_id}"
        path.mkdir(parents=True, exist_ok=True)
        self._attempt_dir[attempt_id] = path
        return path

    def _resolve_task_dir(
        self, target: TaskCenterTaskRecord
    ) -> Path | None:
        role = self._display_role(target)
        if self._is_entry_executor(target):
            return self._run_dir / f"entry_executor_{target.id}"
        attempt_id = target.task_center_attempt_id
        if (
            role in _ATTEMPT_CHILD_ROLES
            and attempt_id
            and attempt_id in self._attempt_dir
        ):
            attempt_dir = self._attempt_dir[attempt_id]
            seq = self._role_seq_counter.get(attempt_id, 0) + 1
            self._role_seq_counter[attempt_id] = seq
            return attempt_dir / f"{seq:02d}_{role}_{target.id}"
        return None

    @staticmethod
    def _display_role(target: TaskCenterTaskRecord) -> str:
        if target.role == "generator" and target.agent_name in {
            "executor",
            "verifier",
        }:
            return str(target.agent_name)
        return str(target.role)

    @staticmethod
    def _is_entry_executor(target: TaskCenterTaskRecord) -> bool:
        """Entry-executor row markers per task_center.entry.coordinator."""
        return (
            target.role == "entry_executor"
            or target.agent_name == "entry_executor"
            or target.spawn_reason == "entry_executor"
        )

    # ------------------------------------------------------------------
    # run.json
    # ------------------------------------------------------------------

    def _write_run_json(self) -> None:
        payload: dict[str, Any] = {
            "task_center_run_id": self._task_center_run_id,
            "scenario_name": self._scenario_name,
            "instance_id": self._instance_id,
            "sandbox_id": self._sandbox_id,
            "started_ts": self._started_ts,
            "finished_ts": self._finished_ts,
            "status": self._status,
        }
        _atomic_write_json(self._run_dir / "run.json", payload)


__all__ = ["AuditRecorder", "PRIMARY_ROLES"]
