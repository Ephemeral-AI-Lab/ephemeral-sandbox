"""AuditRecorder — directory writer + ORM commit listeners.

Wires five SQLAlchemy ``after_insert``/``after_update`` listeners (one per
``WorkflowRecord``/``IterationRecord``/``AttemptRecord``/``TaskCenterTaskRecord``
plus a fifth on ``AgentRunRecord`` for ``agent_run_id`` -> ``task_id``
mapping). Task stream events append conversation-message rows to
``message.jsonl``. Lifecycle rows are mirrored as latest-state ``*.json``
snapshots under a hierarchical run directory, while sandbox subsystem monitor
events are mirrored into ``sandbox_events.jsonl``.
"""

from __future__ import annotations

import os
import time
from collections.abc import Callable
from dataclasses import asdict, dataclass
from datetime import datetime
from pathlib import Path
from typing import Any

from sqlalchemy import event

from audit.jsonl import append_jsonl_event
from task_center_runner.audit.bus import AuditEventBus
from task_center_runner.audit.daemon_event_normalizer import normalize_pulled_event
from task_center_runner.audit.daemon_pull import DaemonAuditPuller, PullerStats
from task_center_runner.audit.events import Event as AuditEvent
from task_center_runner.audit.io import atomic_write_json
from task_center_runner.audit.metrics import MetricsAggregator
from task_center_runner.audit.sandbox_events_sink import RotatingJsonlSink
from db.models.agent_run import AgentRunRecord
from db.models.attempt import AttemptRecord
from db.models.iteration import IterationRecord
from db.models.workflow import WorkflowRecord
from db.models.task_center import TaskCenterTaskRecord
from message.agent_message_recorder import (
    AgentMessageJsonlRecorder,
    clear_recorder,
    register_recorder,
)

DAEMON_AUDIT_PULL_ENABLED_ENV = "EOS_DAEMON_AUDIT_PULL_ENABLED"


def _daemon_audit_pull_enabled() -> bool:
    """V3 §Default-on rollout: opt-out toggle, default True.

    Source precedence (highest first):

    1. ``EOS_DAEMON_AUDIT_PULL_ENABLED`` env var when explicitly set.
    2. ``RunnerConfig.daemon_audit_pull.enabled`` from central config
       (also bindable as ``EOS__RUNNER__DAEMON_AUDIT_PULL__ENABLED``).
    3. Hard default ``True``.

    The recorder already auto-starts the puller whenever a ``sandbox_id`` is
    bound; this gate is the operator escape hatch promoted by Phase 3. The
    runtime invariant in :func:`engine.run_pipeline` refuses to start when
    this gate is off AND ``EOS_AUDIT_STREAM_FALLBACK=false`` AND
    ``EOS_ISOLATED_WORKSPACE_ENABLED=true`` (see V3 README
    §Safety-gate-vs-toggle resolution).
    """
    raw = os.environ.get(DAEMON_AUDIT_PULL_ENABLED_ENV)
    if raw is not None and raw.strip() != "":
        return raw.strip().lower() not in {"false", "0", "no", "off"}
    return _runner_config_daemon_audit_pull_enabled()


def _runner_config_daemon_audit_pull_enabled() -> bool:
    """Read ``RunnerConfig.daemon_audit_pull.enabled`` defensively.

    Central config may not be initialized in unit-test contexts where the
    env-var fallback is the source of truth; we treat any access failure
    as "use the default (True)" rather than raising into the recorder.
    """
    try:
        from config import get_central_config

        return bool(
            get_central_config().runner.daemon_audit_pull.enabled
        )
    except Exception:  # noqa: BLE001 — central config is best-effort here
        return True


PRIMARY_ROLES: frozenset[str] = frozenset(
    {"planner", "executor", "verifier", "evaluator"}
)

# Roles which earn an ``NN_<role>_<task_id>`` directory under the parent
# attempt — superset of the primary message-recorder allowlist (we still
# want the ``task.json`` snapshot for ``generator`` rows).
_ATTEMPT_CHILD_ROLES: frozenset[str] = frozenset(
    {"planner", "executor", "verifier", "evaluator", "generator"}
)


def _isoformat(value: datetime | None) -> str | None:
    return value.isoformat() if value is not None else None


def _serialize_workflow(record: WorkflowRecord) -> dict[str, Any]:
    return {
        "id": record.id,
        "task_center_run_id": record.task_center_run_id,
        "origin_kind": record.origin_kind,
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
        "workflow_id": record.workflow_id,
        "sequence_no": record.sequence_no,
        "creation_reason": record.creation_reason,
        "goal": record.goal,
        "attempt_budget": record.attempt_budget,
        "status": record.status,
        "attempt_ids": list(record.attempt_ids or []),
        "deferred_goal": record.deferred_goal,
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
        "deferred_goal": record.deferred_goal,
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
        "context_message": record.context_message,
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
        coding_plan_mode_active: bool = False,
    ) -> None:
        self._run_dir = Path(run_dir)
        self._task_center_run_id = task_center_run_id
        self._bus = bus
        self._primary_roles = frozenset(primary_roles)
        self._scenario_name = scenario_name
        self._instance_id = instance_id
        self._sandbox_id = sandbox_id
        self._coding_plan_mode_active = coding_plan_mode_active  # plan §A11

        self._workflow_dir: dict[str, Path] = {}
        self._iteration_dir: dict[str, Path] = {}
        self._attempt_dir: dict[str, Path] = {}
        self._task_dir: dict[str, Path] = {}
        self._task_recorder: dict[str, AgentMessageJsonlRecorder] = {}
        self._agent_run_to_task: dict[str, tuple[str, str]] = {}

        self._workflow_seq_counter: int = 0
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

        self._daemon_audit_puller: DaemonAuditPuller | None = None
        self._sandbox_events_sink: RotatingJsonlSink | None = None
        self._daemon_audit_boot_epoch_id: int | None = None
        # Stashed AFTER `puller.stop()` so final-drain `events_pulled` /
        # `final_cursor` land in the perf-report's §11. Engine.run_pipeline
        # reads this post-aclose and threads it into _write_perf_report_safe.
        self._final_daemon_audit_puller_stats: dict[str, Any] | None = None

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
        entry = self._agent_run_to_task.get(agent_run_id)
        if entry is None:
            return None
        _, task_id = entry
        return self._task_recorder.get(task_id)

    def start(self) -> None:
        """Register the 5 SQLAlchemy listeners and write run.json.

        Phase 3 deferral D12: the safety check that refuses dual-disable
        when isolated_workspace is enabled lives here so non-engine code
        paths (ad-hoc scripts, host adapters) inherit the guarantee. The
        engine entrypoint calls the same helper upstream so misconfig is
        caught before any sandbox is provisioned.
        """
        # Lazy import to avoid a recorder→engine import cycle. The
        # function is a pure env-var read; no heavy state.
        from task_center_runner.core.engine import (
            _refuse_dual_disable_when_isolated_workspace_enabled,
        )

        _refuse_dual_disable_when_isolated_workspace_enabled()

        self._run_dir.mkdir(parents=True, exist_ok=True)
        self._started_ts = time.time()
        self._status = "running"

        self._register(
            WorkflowRecord,
            "after_insert",
            lambda mapper, connection, target: self._handle_workflow(target),
        )
        self._register(
            WorkflowRecord,
            "after_update",
            lambda mapper, connection, target: self._handle_workflow(target),
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

        self._maybe_auto_start_daemon_audit_puller()
        self._write_run_json()

    def _maybe_auto_start_daemon_audit_puller(self) -> None:
        """Auto-construct + start the daemon audit puller when ``sandbox_id`` is set.

        Slice 6 contract: ``AuditRecorder.start()`` flips
        ``sandbox_events.jsonl`` from the stream-bridge to the pull path
        whenever it has a sandbox to point at. Callers that need explicit
        control (custom transport, custom pull callable) may still use
        :meth:`attach_daemon_audit_puller` — auto-start is a no-op once a
        puller has been attached.

        Phase 3 §Default-on rollout: ``EOS_DAEMON_AUDIT_PULL_ENABLED=false``
        skips auto-start so operators can fall back to the stream-bridge
        without code changes (e.g. when the overhead gate fails post-ship).
        """
        if not self._sandbox_id or self._daemon_audit_puller is not None:
            return
        if not _daemon_audit_pull_enabled():
            return
        try:
            import asyncio

            asyncio.get_running_loop()
        except RuntimeError:
            # Recorder constructed outside an event loop (test fixtures,
            # synchronous host paths). Caller can still attach manually.
            return

        sandbox_id = self._sandbox_id

        async def _pull(after_seq: int, limit: int) -> dict[str, Any]:
            from sandbox.api.daemon_audit import audit_pull as _api_audit_pull

            return await _api_audit_pull(
                sandbox_id,
                after_seq=after_seq,
                limit=limit,
            )

        try:
            self.attach_daemon_audit_puller(pull=_pull)
        except Exception:  # noqa: BLE001 — audit never breaks the host path
            self._daemon_audit_puller = None
            self._sandbox_events_sink = None

    def attach_daemon_audit_puller(
        self,
        *,
        pull: Callable[[int, int], Any],
        sink_path: Path | None = None,
    ) -> DaemonAuditPuller:
        """Wire a ``DaemonAuditPuller`` whose events feed the rotating sink.

        Slice 6: ``sandbox_events.jsonl`` switches from the stream-bridge
        (``_record_sandbox_event``) to the daemon-ring pull path. The
        stream-bridge stays as fallback until follow-up FU#1 retires it.

        ``pull`` is expected to return a coroutine resolving to the daemon
        pull RPC response. The caller (host runtime) owns the sandbox_id
        binding; the recorder owns the sink + final-drain coordination.
        """
        if self._daemon_audit_puller is not None:
            return self._daemon_audit_puller
        sink_target = sink_path or (self._run_dir / "sandbox_events.jsonl")
        sink = RotatingJsonlSink(sink_target)
        self._sandbox_events_sink = sink

        def _emit(events: list[dict[str, Any]], response: dict[str, Any]) -> None:
            snapshot = response.get("snapshot") or {}
            daemon = snapshot.get("daemon") or {}
            boot_epoch_id = daemon.get("boot_epoch_id")
            if isinstance(boot_epoch_id, int):
                self._daemon_audit_boot_epoch_id = boot_epoch_id
            for event_payload in events:
                normalized = normalize_pulled_event(
                    event_payload,
                    boot_epoch_id=self._daemon_audit_boot_epoch_id,
                    task_center_run_id=self._task_center_run_id,
                )
                sink.append_event(normalized)

        puller = DaemonAuditPuller(pull, emit=_emit)
        self._daemon_audit_puller = puller
        puller.start()
        return puller

    def daemon_audit_puller_stats(self) -> PullerStats | None:
        """Return the live puller stats if a puller has been attached."""
        return None if self._daemon_audit_puller is None else self._daemon_audit_puller.stats

    def final_daemon_audit_puller_stats(self) -> dict[str, Any] | None:
        """Final puller stats captured post-``stop()`` (Phase 3).

        Snapshot lives here so the perf-report's §11 sees ``final_cursor``
        and any final-drain ``events_pulled`` that landed AFTER the puller
        loop exited. Returns ``None`` if no puller was ever attached.
        """
        return self._final_daemon_audit_puller_stats

    async def stop_daemon_audit_puller(self) -> None:
        """Stop the attached puller and run its final drain.

        Live callers should normally use :meth:`aclose` (Closer F) which
        chains this method with the sync dispose body in the correct
        order. This method stays public for tests that want to drain
        without tearing the whole recorder down.

        Final stats are stashed via
        :meth:`final_daemon_audit_puller_stats` AFTER ``await
        puller.stop()`` so the final drain's ``events_pulled`` /
        ``final_cursor`` land in the snapshot (Phase 3 §11).
        """
        puller = self._daemon_audit_puller
        if puller is not None:
            await puller.stop()
            self._final_daemon_audit_puller_stats = puller.stats.as_dict()
        self._daemon_audit_puller = None

    async def aclose(self) -> None:
        """Single async teardown path for live callers (Closer F).

        Awaits the daemon-audit puller's final drain (if attached) and
        then runs the synchronous dispose body. Live callers (the runner)
        MUST use this. Sync :meth:`dispose` is preserved for test stubs
        that never attach a puller — see its docstring for the contract.
        """
        await self.stop_daemon_audit_puller()
        self._dispose_sync()

    def dispose(self) -> None:
        """Synchronous teardown forwarder.

        Raises ``RuntimeError`` when a daemon-audit puller is still
        attached — live runtimes must call :meth:`aclose` so the puller's
        final drain runs before the sink + listeners flush. Test stubs
        that never attach a puller continue to work unchanged.
        """
        if self._daemon_audit_puller is not None:
            raise RuntimeError(
                "AuditRecorder.dispose() cannot reclaim an active daemon-"
                "audit puller; call AuditRecorder.aclose() instead."
            )
        self._dispose_sync()

    def _dispose_sync(self) -> None:
        """Shared sync teardown body for :meth:`dispose` and :meth:`aclose`."""
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

        for agent_run_id, (agent_name, _) in list(self._agent_run_to_task.items()):
            clear_recorder(agent_name, agent_run_id)

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

    def _handle_workflow(self, target: WorkflowRecord) -> None:
        if (
            self._task_center_run_id
            and target.task_center_run_id != self._task_center_run_id
        ):
            return
        workflow_dir = self._ensure_workflow_dir(target.id)
        _atomic_write_json(workflow_dir / "workflow.json", _serialize_workflow(target))

    def _handle_iteration(self, target: IterationRecord) -> None:
        workflow_dir = self._workflow_dir.get(target.workflow_id)
        if workflow_dir is None:
            return
        iteration_dir = self._ensure_iteration_dir(
            target.workflow_id, target.id, workflow_dir
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
            primary = display_role in self._primary_roles
            if primary:
                recorder = AgentMessageJsonlRecorder(
                    task_dir / "message.jsonl",
                    base_event={
                        "task_id": target.id,
                        "task_center_run_id": self._task_center_run_id,
                    },
                )
                self._task_recorder[target.id] = recorder
                for agent_run_id, (agent_name, task_id) in self._agent_run_to_task.items():
                    if task_id == target.id:
                        register_recorder(agent_name, agent_run_id, recorder)
        _atomic_write_json(task_dir / "task.json", _serialize_task(target))

    def _handle_agent_run(self, target: AgentRunRecord) -> None:
        self._agent_run_to_task[target.id] = (target.agent_name, target.task_id)
        recorder = self._task_recorder.get(target.task_id)
        if recorder is not None:
            register_recorder(target.agent_name, target.id, recorder)

    def _record_sandbox_event(self, audit_event: AuditEvent) -> None:
        if not (
            audit_event.type.value.startswith("sandbox_")
            or audit_event.type.name.startswith("SANDBOX_")
        ):
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

    def _ensure_workflow_dir(self, workflow_id: str) -> Path:
        cached = self._workflow_dir.get(workflow_id)
        if cached is not None:
            return cached
        self._workflow_seq_counter += 1
        seq = self._workflow_seq_counter
        path = self._run_dir / f"workflow_{seq:02d}_{workflow_id}"
        path.mkdir(parents=True, exist_ok=True)
        self._workflow_dir[workflow_id] = path
        return path

    def _ensure_iteration_dir(
        self, workflow_id: str, iteration_id: str, workflow_dir: Path
    ) -> Path:
        cached = self._iteration_dir.get(iteration_id)
        if cached is not None:
            return cached
        seq = self._iteration_seq_counter.get(workflow_id, 0) + 1
        self._iteration_seq_counter[workflow_id] = seq
        path = workflow_dir / f"iteration_{seq:02d}_{iteration_id}"
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
            "coding_plan_mode_active": self._coding_plan_mode_active,
        }
        _atomic_write_json(self._run_dir / "run.json", payload)


__all__ = ["AuditRecorder", "PRIMARY_ROLES"]
