"""Runner-side puller for the daemon audit ring.

Pulls events from :mod:`sandbox.daemon.audit_buffer` via the
``api.audit.{pull,snapshot}`` RPCs, with adaptive cadence + floor enforcement,
final drain on stop, and daemon-restart epoch handling.

See ``docs/daemon-audit-pull-consolidation-v3/phase-2-emitters-and-puller.md``
and the README §Adaptive cadence policy section for the contract.

The floor itself is a runner-side concern: Phase 1 shipped a daemon-side
``api.audit.reset_floor`` stub that just checks the env gate; this module owns
the actual floor state (Phase 1 §Deferred).
"""

from __future__ import annotations

import asyncio
import logging
import os
import time
from collections.abc import Awaitable, Callable
from dataclasses import dataclass, field
from typing import Any

logger = logging.getLogger(__name__)

DEFAULT_FLOOR_MS = 100
MAX_FLOOR_MS = 1000
PRESSURE_ESCALATION_THRESHOLD = 0.8
PRESSURE_ESCALATION_STREAK = 3
PRESSURE_ESCALATION_FACTOR = 1.5
ACTIVE_TARGET_MS = 1000
IDLE_TARGET_MS = 5000
ISOLATED_TARGET_MS = 500
PRESSURE_TARGET_MS = 250
FINAL_DRAIN_CAP_MS = 3000
DEFAULT_PULL_LIMIT = 1000


PullFn = Callable[[int, int], Awaitable[dict[str, Any]]]
EmitFn = Callable[[list[dict[str, Any]], dict[str, Any]], None]


@dataclass
class PullerStats:
    pull_count: int = 0
    empty_pull_count: int = 0
    events_pulled: int = 0
    pull_error_count: int = 0
    dropped_event_count: int = 0
    lost_before_seq: int = 0
    max_buffer_pressure: float = 0.0
    final_cursor: int = -1
    floor_raises: int = 0
    daemon_restarts_observed: int = 0
    pull_ms_samples: list[float] = field(default_factory=list)

    def record_pull_ms(self, value: float) -> None:
        # Bounded ring; we only keep the last 1024 samples for percentile math.
        if len(self.pull_ms_samples) >= 1024:
            self.pull_ms_samples.pop(0)
        self.pull_ms_samples.append(value)

    def percentiles(self) -> dict[str, float]:
        if not self.pull_ms_samples:
            return {"p50": 0.0, "p95": 0.0, "p99": 0.0}
        ordered = sorted(self.pull_ms_samples)
        return {
            "p50": _percentile(ordered, 50),
            "p95": _percentile(ordered, 95),
            "p99": _percentile(ordered, 99),
        }

    def as_dict(self) -> dict[str, Any]:
        return {
            "pull_count": self.pull_count,
            "empty_pull_count": self.empty_pull_count,
            "events_pulled": self.events_pulled,
            "pull_error_count": self.pull_error_count,
            "dropped_event_count": self.dropped_event_count,
            "lost_before_seq": self.lost_before_seq,
            "max_buffer_pressure": self.max_buffer_pressure,
            "final_cursor": self.final_cursor,
            "floor_raises": self.floor_raises,
            "daemon_restarts_observed": self.daemon_restarts_observed,
            "pull_ms": self.percentiles(),
        }


def _percentile(ordered: list[float], pct: float) -> float:
    if not ordered:
        return 0.0
    k = max(1, int(round(pct / 100.0 * len(ordered))))
    return float(ordered[min(k, len(ordered)) - 1])


class DaemonAuditPuller:
    """Polls the daemon audit ring at an adaptive cadence.

    Concurrency: the puller runs as a single asyncio task. Callers drive
    cadence by setting ``isolated_active``; pressure-based escalation happens
    automatically. ``stop()`` triggers a bounded final drain so the recorder
    can dispose without losing tail events.
    """

    def __init__(
        self,
        pull: PullFn,
        *,
        emit: EmitFn,
        floor_ms: int | None = None,
        active_target_ms: int = ACTIVE_TARGET_MS,
        idle_target_ms: int = IDLE_TARGET_MS,
        isolated_target_ms: int = ISOLATED_TARGET_MS,
        pressure_target_ms: int = PRESSURE_TARGET_MS,
        pull_limit: int = DEFAULT_PULL_LIMIT,
    ) -> None:
        # Precedence (V3 Phase 3 deferral D13):
        # 1. Explicit kwarg (test fixtures).
        # 2. ``EOS_DAEMON_AUDIT_PULL_FLOOR_MS`` env var when set.
        # 3. ``RunnerConfig.daemon_audit_pull.floor_ms`` from central config.
        # 4. Hard default ``DEFAULT_FLOOR_MS``.
        if floor_ms is not None:
            resolved_floor = floor_ms
        elif os.environ.get("EOS_DAEMON_AUDIT_PULL_FLOOR_MS", "").strip():
            resolved_floor = _env_int(
                "EOS_DAEMON_AUDIT_PULL_FLOOR_MS", DEFAULT_FLOOR_MS
            )
        else:
            resolved_floor = _runner_config_floor_ms()
        self._default_floor_ms = resolved_floor
        self._floor_ms = self._default_floor_ms
        self._active_target_ms = active_target_ms
        self._idle_target_ms = idle_target_ms
        self._isolated_target_ms = isolated_target_ms
        self._pressure_target_ms = pressure_target_ms
        self._pull = pull
        self._emit = emit
        self._pull_limit = pull_limit

        self._cursor = -1
        self._known_epoch_id: int | None = None
        self._stats = PullerStats(final_cursor=-1)
        self._pressure_streak = 0

        self._isolated_active = False
        self._has_inflight = False

        self._task: asyncio.Task[None] | None = None
        self._stop_event: asyncio.Event | None = None
        self._stopped = False

    # ------------------------------------------------------------------
    # Lifecycle
    # ------------------------------------------------------------------

    def start(self) -> None:
        if self._task is not None:
            return
        loop = asyncio.get_event_loop()
        self._stop_event = asyncio.Event()
        self._task = loop.create_task(self._run())

    async def stop(self) -> None:
        if self._task is None:
            return
        assert self._stop_event is not None
        self._stop_event.set()
        try:
            await self._task
        finally:
            self._task = None
            self._stop_event = None
            self._stopped = True

    # ------------------------------------------------------------------
    # Cadence knobs (called by host orchestration)
    # ------------------------------------------------------------------

    def set_isolated_active(self, active: bool) -> None:
        self._isolated_active = bool(active)

    def set_inflight(self, has_inflight: bool) -> None:
        self._has_inflight = bool(has_inflight)

    def reset_floor(self) -> None:
        """Operator escape hatch — called when daemon ``reset_floor`` succeeds."""
        self._floor_ms = self._default_floor_ms

    # ------------------------------------------------------------------
    # Stats
    # ------------------------------------------------------------------

    @property
    def stats(self) -> PullerStats:
        return self._stats

    @property
    def floor_ms(self) -> int:
        return self._floor_ms

    # ------------------------------------------------------------------
    # Main loop
    # ------------------------------------------------------------------

    async def _run(self) -> None:
        assert self._stop_event is not None
        while not self._stop_event.is_set():
            await self._pull_once()
            interval_ms = self._compute_interval_ms(final_drain=False)
            try:
                await asyncio.wait_for(
                    self._stop_event.wait(),
                    timeout=interval_ms / 1000.0,
                )
            except asyncio.TimeoutError:
                continue
            else:
                break
        await self._final_drain()

    async def _pull_once(self) -> bool:
        """Drain whatever is currently retrievable. Returns True if any events seen."""
        any_events = False
        while True:
            start = time.monotonic()
            try:
                response = await self._pull(self._cursor, self._pull_limit)
            except Exception:  # noqa: BLE001 — never block on transient failures
                self._stats.pull_error_count += 1
                logger.warning("daemon audit pull failed", exc_info=True)
                return any_events
            elapsed_ms = (time.monotonic() - start) * 1000.0
            self._stats.pull_count += 1
            self._stats.record_pull_ms(elapsed_ms)
            self._observe_buffer(response.get("buffer") or {})
            self._observe_epoch(response)
            events = response.get("events") or []
            cursor_block = response.get("cursor") or {}
            if events:
                any_events = True
                self._stats.events_pulled += len(events)
                self._emit(list(events), response)
                cursor_seq = cursor_block.get("after_seq")
                if isinstance(cursor_seq, int):
                    self._cursor = max(self._cursor, cursor_seq)
                    self._stats.final_cursor = self._cursor
                if len(events) < self._pull_limit:
                    return any_events
                continue
            self._stats.empty_pull_count += 1
            return any_events

    async def _final_drain(self) -> None:
        deadline = time.monotonic() + FINAL_DRAIN_CAP_MS / 1000.0
        while time.monotonic() < deadline:
            had_events = await self._pull_once()
            if not had_events:
                return

    def _observe_buffer(self, buffer_block: dict[str, Any]) -> None:
        try:
            pressure = float(buffer_block.get("pressure") or 0.0)
        except (TypeError, ValueError):
            pressure = 0.0
        if pressure > self._stats.max_buffer_pressure:
            self._stats.max_buffer_pressure = pressure
        dropped = int(buffer_block.get("dropped_event_count") or 0)
        if dropped > self._stats.dropped_event_count:
            self._stats.dropped_event_count = dropped
        lost = int(buffer_block.get("lost_before_seq") or 0)
        if lost > self._stats.lost_before_seq:
            self._stats.lost_before_seq = lost
        if pressure > PRESSURE_ESCALATION_THRESHOLD:
            self._pressure_streak += 1
            if self._pressure_streak >= PRESSURE_ESCALATION_STREAK:
                self._escalate_floor()
                self._pressure_streak = 0
        else:
            self._pressure_streak = 0

    def _observe_epoch(self, response: dict[str, Any]) -> None:
        snapshot = response.get("snapshot") or {}
        daemon = snapshot.get("daemon") or {}
        boot_epoch_id = daemon.get("boot_epoch_id")
        if not isinstance(boot_epoch_id, int):
            return
        if self._known_epoch_id is None:
            self._known_epoch_id = boot_epoch_id
            return
        if boot_epoch_id == self._known_epoch_id:
            return
        # Epoch boundary observed — daemon restarted.
        previous = self._known_epoch_id
        self._known_epoch_id = boot_epoch_id
        self._stats.daemon_restarts_observed += 1
        self._cursor = -1
        self._stats.final_cursor = -1
        synthetic = [
            {
                "seq": -1,
                "lane": "critical",
                "type": "daemon.restart_observed",
                "payload": {
                    "daemon": {
                        "previous_epoch_id": previous,
                        "new_epoch_id": boot_epoch_id,
                    }
                },
            }
        ]
        self._emit(synthetic, response)

    def _escalate_floor(self) -> None:
        new_floor = min(
            MAX_FLOOR_MS, int(self._floor_ms * PRESSURE_ESCALATION_FACTOR) + 1
        )
        if new_floor <= self._floor_ms:
            return
        self._floor_ms = new_floor
        self._stats.floor_raises += 1

    def _compute_interval_ms(self, *, final_drain: bool) -> int:
        if final_drain:
            return self._floor_ms
        if self._stats.max_buffer_pressure >= PRESSURE_ESCALATION_THRESHOLD:
            target = self._pressure_target_ms
        elif self._isolated_active:
            target = self._isolated_target_ms
        elif self._has_inflight:
            target = self._active_target_ms
        else:
            target = self._idle_target_ms
        return max(self._floor_ms, target)


def _env_int(name: str, default: int) -> int:
    raw = os.environ.get(name)
    if not raw:
        return default
    try:
        return max(1, int(raw))
    except (TypeError, ValueError):
        return default


def _runner_config_floor_ms() -> int:
    """Read ``RunnerConfig.daemon_audit_pull.floor_ms`` defensively.

    Falls back to :data:`DEFAULT_FLOOR_MS` when central config is not
    initialised (e.g. unit-test contexts) so the puller stays usable.
    """
    try:
        from config import get_central_config

        value = int(
            get_central_config().runner.daemon_audit_pull.floor_ms
        )
        return max(1, value)
    except Exception:  # noqa: BLE001 — central config is best-effort here
        return DEFAULT_FLOOR_MS


__all__ = [
    "DaemonAuditPuller",
    "PullerStats",
    "DEFAULT_FLOOR_MS",
    "MAX_FLOOR_MS",
    "PRESSURE_ESCALATION_THRESHOLD",
    "PRESSURE_ESCALATION_STREAK",
]
