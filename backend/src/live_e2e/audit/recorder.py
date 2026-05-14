"""AuditRecorder — directory writer + ORM commit listeners.

Wires five SQLAlchemy ``after_insert``/``after_update`` listeners (one per
``MissionRecord``/``EpisodeRecord``/``AttemptRecord``/``TaskCenterTaskRecord``
plus a fifth on ``AgentRunRecord`` for ``agent_run_id`` -> ``task_id``
mapping). Task stream events append conversation-message rows to
``message.jsonl``. Lifecycle rows are mirrored as latest-state ``*.json``
snapshots under a hierarchical run directory, while sandbox subsystem monitor
events are mirrored into ``sandbox_events.jsonl``.
"""

from __future__ import annotations

import json
import os
import time
from collections.abc import Callable
from dataclasses import asdict, dataclass
from datetime import datetime
from pathlib import Path
from typing import Any

from sqlalchemy import event

from audit.jsonl import append_jsonl_event
from live_e2e.audit.bus import AuditEventBus
from live_e2e.audit.events import Event as AuditEvent
from live_e2e.audit.metrics import MetricsAggregator
from live_e2e.audit.performance_report import write_performance_reports
from db.models.agent_run import AgentRunRecord
from db.models.attempt import AttemptRecord
from db.models.episode import EpisodeRecord
from db.models.mission import MissionRecord
from db.models.task_center import TaskCenterTaskRecord
from message.agent_message_recorder import AgentMessageJsonlRecorder


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


def _serialize_mission(record: MissionRecord) -> dict[str, Any]:
    return {
        "id": record.id,
        "task_center_run_id": record.task_center_run_id,
        "requested_by_task_id": record.requested_by_task_id,
        "goal": record.goal,
        "status": record.status,
        "episode_ids": list(record.episode_ids or []),
        "final_outcome": record.final_outcome,
        "created_at": _isoformat(record.created_at),
        "updated_at": _isoformat(record.updated_at),
        "closed_at": _isoformat(record.closed_at),
    }


def _serialize_episode(record: EpisodeRecord) -> dict[str, Any]:
    return {
        "id": record.id,
        "mission_id": record.mission_id,
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
        "episode_id": record.episode_id,
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


def _atomic_write_json(path: Path, data: dict[str, Any]) -> None:
    """Write ``data`` as JSON via tmp + os.replace."""
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp_path = path.with_suffix(path.suffix + ".tmp")
    encoded = json.dumps(data, default=str, ensure_ascii=False)
    tmp_path.write_text(encoded, encoding="utf-8")
    os.replace(tmp_path, path)


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

        self._mission_dir: dict[str, Path] = {}
        self._episode_dir: dict[str, Path] = {}
        self._attempt_dir: dict[str, Path] = {}
        self._task_dir: dict[str, Path] = {}
        self._task_recorder: dict[str, AgentMessageJsonlRecorder] = {}
        self._agent_run_to_task: dict[str, str] = {}

        self._mission_seq_counter: int = 0
        self._episode_seq_counter: dict[str, int] = {}
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
            MissionRecord,
            "after_insert",
            lambda mapper, connection, target: self._handle_mission(target),
        )
        self._register(
            MissionRecord,
            "after_update",
            lambda mapper, connection, target: self._handle_mission(target),
        )
        self._register(
            EpisodeRecord,
            "after_insert",
            lambda mapper, connection, target: self._handle_episode(target),
        )
        self._register(
            EpisodeRecord,
            "after_update",
            lambda mapper, connection, target: self._handle_episode(target),
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

        self._finished_ts = time.time()
        if self._status == "running":
            self._status = "finished"
        self._write_run_json()
        _atomic_write_json(self._run_dir / "metrics.json", self._metrics.snapshot())
        write_performance_reports(
            self._run_dir,
            self._metrics.performance_snapshot(),
        )

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

    def _handle_mission(self, target: MissionRecord) -> None:
        if (
            self._task_center_run_id
            and target.task_center_run_id != self._task_center_run_id
        ):
            return
        mission_dir = self._ensure_mission_dir(target.id)
        _atomic_write_json(mission_dir / "mission.json", _serialize_mission(target))

    def _handle_episode(self, target: EpisodeRecord) -> None:
        mission_dir = self._mission_dir.get(target.mission_id)
        if mission_dir is None:
            return
        episode_dir = self._ensure_episode_dir(
            target.mission_id, target.id, mission_dir
        )
        _atomic_write_json(episode_dir / "episode.json", _serialize_episode(target))

    def _handle_attempt(self, target: AttemptRecord) -> None:
        episode_dir = self._episode_dir.get(target.episode_id)
        if episode_dir is None:
            return
        attempt_dir = self._ensure_attempt_dir(
            target.episode_id, target.id, episode_dir
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
                self._task_recorder[target.id] = AgentMessageJsonlRecorder(
                    task_dir / "message.jsonl",
                    base_event={
                        "task_id": target.id,
                        "task_center_run_id": self._task_center_run_id,
                    },
                )
        _atomic_write_json(task_dir / "task.json", _serialize_task(target))

    def _handle_agent_run(self, target: AgentRunRecord) -> None:
        self._agent_run_to_task[target.id] = target.task_id

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

    def _ensure_mission_dir(self, mission_id: str) -> Path:
        cached = self._mission_dir.get(mission_id)
        if cached is not None:
            return cached
        self._mission_seq_counter += 1
        seq = self._mission_seq_counter
        path = self._run_dir / f"mission_{seq:02d}_{mission_id}"
        path.mkdir(parents=True, exist_ok=True)
        self._mission_dir[mission_id] = path
        return path

    def _ensure_episode_dir(
        self, mission_id: str, episode_id: str, mission_dir: Path
    ) -> Path:
        cached = self._episode_dir.get(episode_id)
        if cached is not None:
            return cached
        seq = self._episode_seq_counter.get(mission_id, 0) + 1
        self._episode_seq_counter[mission_id] = seq
        path = mission_dir / f"episode_{seq:02d}_{episode_id}"
        path.mkdir(parents=True, exist_ok=True)
        self._episode_dir[episode_id] = path
        return path

    def _ensure_attempt_dir(
        self, episode_id: str, attempt_id: str, episode_dir: Path
    ) -> Path:
        cached = self._attempt_dir.get(attempt_id)
        if cached is not None:
            return cached
        seq = self._attempt_seq_counter.get(episode_id, 0) + 1
        self._attempt_seq_counter[episode_id] = seq
        path = episode_dir / f"attempt_{seq:02d}_{attempt_id}"
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
