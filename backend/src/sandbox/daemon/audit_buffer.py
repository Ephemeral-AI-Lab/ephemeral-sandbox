"""Bounded daemon-side audit ring with pull / snapshot semantics.

Phase 1 of `docs/daemon-audit-pull-consolidation-v3/`. The daemon never writes
audit to disk; consumers pull from this in-memory ring via
``api.audit.{pull,snapshot,reset_floor}``.

Schema identifier (frozen at v1): :data:`SCHEMA_VERSION` =
``sandbox.daemon.audit.pull.v1``.

Subsystem section keys (frozen at v1)::

    daemon, layer_stack, overlay_workspace, occ, isolated_workspace,
    os_resource, plugin, background_tool, tool_call

Lane assignment (see README §Lane assignment for full table). Eviction
priority is ``sample → normal → critical``; critical-lane events survive
sample-lane pressure.

Event families (single source of truth)::

    daemon.{started, stopped, audit_buffer_pressure}        [critical]
    daemon.restart_observed                                 [critical]
    isolated_workspace.{entered, exited, evicted,
        orphan_check_completed, orphan_reaped}              [critical]
    isolated_workspace.sampled                              [sample]
    overlay_workspace.{mounted, published, cleaned,
        cleanup_failed}                                     [critical]
    layer_stack.{squash_triggered, squash_completed,
        squash_failed}                                      [critical]
    layer_stack.{lease_requested, lease_acquired,
        lease_released, lock_acquired,
        snapshot_prepared}                                  [normal]
    occ.conflict_rejected                                   [critical]
    occ.{changeset_prepared, transaction_lock_acquired,
        apply_committed, publish_layer}                     [normal]
    os_resource.sampled                                     [sample]
    plugin.{tool_invoked, tool_completed, error}            [normal]
    plugin.peak_resident_sampled                            [sample]
    background_tool.{started, completed, failed,
        cancelled, delivered}                               [normal]
    background_tool.heartbeat                               [sample]
    tool_call.{started, finished}                           [normal]
    tool_call.phase                                         [sample]

Lane changes are a v2 break (consumer rejects unknown majors).
"""

from __future__ import annotations

import json
import threading
import time
from collections import deque
from dataclasses import dataclass
from typing import Any, Literal

SCHEMA_VERSION = "sandbox.daemon.audit.pull.v1"

Lane = Literal["critical", "normal", "sample"]

_LANES: tuple[Lane, ...] = ("critical", "normal", "sample")
_EVICTION_ORDER: tuple[Lane, ...] = ("sample", "normal", "critical")

_DEFAULT_MAX_EVENTS = 50_000
_DEFAULT_MAX_BYTES = 8 * 1024 * 1024


@dataclass
class BufferedEvent:
    seq: int
    lane: Lane
    encoded_bytes: int
    payload: dict[str, Any]


def _encoded_size(payload: dict[str, Any]) -> int:
    try:
        return len(json.dumps(payload, default=str).encode("utf-8"))
    except (TypeError, ValueError):
        return len(repr(payload).encode("utf-8"))


@dataclass
class _LaneCounters:
    events: int = 0
    bytes: int = 0
    dropped: int = 0


@dataclass
class _PressureTracker:
    """Edge-triggered pressure cross detector for `daemon.audit_buffer_pressure`."""

    threshold: float = 0.8
    above: bool = False

    def cross_rising(self, pressure: float) -> bool:
        if pressure >= self.threshold and not self.above:
            self.above = True
            return True
        if pressure < self.threshold:
            self.above = False
        return False


@dataclass
class _Snapshot:
    retained_events: int
    retained_bytes: int
    max_events: int
    max_bytes: int
    pressure: float
    dropped_event_count: int
    dropped_event_count_by_lane: dict[str, int]
    lost_before_seq: int
    next_seq: int
    boot_epoch_id: int


class AuditBuffer:
    """Bounded in-memory ring buffer with lane-priority eviction.

    Concurrency: a single ``threading.Lock`` guards all state. Daemon
    dispatcher is asyncio (single thread) plus boot-time emitters that may
    fire before the loop starts; a plain lock is correct for both paths.
    """

    def __init__(
        self,
        *,
        max_events: int = _DEFAULT_MAX_EVENTS,
        max_bytes: int = _DEFAULT_MAX_BYTES,
        boot_epoch_id: int | None = None,
        pressure_threshold: float = 0.8,
    ) -> None:
        if max_events <= 0 or max_bytes <= 0:
            raise ValueError("max_events and max_bytes must be positive")
        self._max_events = max_events
        self._max_bytes = max_bytes
        self._boot_epoch_id = (
            boot_epoch_id if boot_epoch_id is not None else time.monotonic_ns()
        )
        self._lock = threading.Lock()
        self._next_seq = 0
        self._lost_before_seq = 0
        self._dropped_total = 0
        self._lanes: dict[Lane, deque[BufferedEvent]] = {
            lane: deque() for lane in _LANES
        }
        self._counters: dict[Lane, _LaneCounters] = {
            lane: _LaneCounters() for lane in _LANES
        }
        self._tracker = _PressureTracker(threshold=pressure_threshold)
        self._all: deque[BufferedEvent] = deque()
        self._on_pressure_cross: list[Any] = []

    @property
    def boot_epoch_id(self) -> int:
        return self._boot_epoch_id

    def register_pressure_cross_callback(self, callback: Any) -> None:
        """Register a callback fired on rising 0.8 pressure crossings.

        Callback is invoked OUTSIDE the buffer lock with the snapshot dict, to
        let it re-enter ``append`` (e.g. emit `daemon.audit_buffer_pressure`)
        without deadlock or recursion.
        """
        self._on_pressure_cross.append(callback)

    def append(self, event: dict[str, Any], lane: Lane = "normal") -> int:
        if lane not in _LANES:
            raise ValueError(f"unknown lane: {lane!r}")
        encoded = _encoded_size(event)
        crossed_snapshot: dict[str, Any] | None = None
        with self._lock:
            seq = self._next_seq
            self._next_seq += 1
            payload = dict(event)
            payload["seq"] = seq
            payload["lane"] = lane
            buf_event = BufferedEvent(
                seq=seq,
                lane=lane,
                encoded_bytes=encoded,
                payload=payload,
            )
            self._lanes[lane].append(buf_event)
            self._all.append(buf_event)
            self._counters[lane].events += 1
            self._counters[lane].bytes += encoded
            self._enforce_caps_locked()
            pressure = self._pressure_locked()
            if self._tracker.cross_rising(pressure):
                crossed_snapshot = self._snapshot_locked().__dict__
        if crossed_snapshot is not None:
            for cb in list(self._on_pressure_cross):
                try:
                    cb(crossed_snapshot)
                except Exception:
                    pass
        return seq

    def pull(self, after_seq: int = -1, limit: int = 1000) -> dict[str, Any]:
        if limit <= 0:
            limit = 1
        with self._lock:
            out: list[dict[str, Any]] = []
            for ev in self._all:
                if ev.seq <= after_seq:
                    continue
                out.append(dict(ev.payload))
                if len(out) >= limit:
                    break
            snap = self._snapshot_locked()
            next_cursor = out[-1]["seq"] if out else after_seq
            response = {
                "schema": SCHEMA_VERSION,
                "cursor": {
                    "after_seq": next_cursor,
                    "lost_before_seq": snap.lost_before_seq,
                },
                "buffer": self._buffer_block(snap),
                "snapshot": self._snapshot_block(snap),
                "events": out,
            }
            return response

    def snapshot(self) -> dict[str, Any]:
        with self._lock:
            snap = self._snapshot_locked()
            return {
                "schema": SCHEMA_VERSION,
                "buffer": self._buffer_block(snap),
                "snapshot": self._snapshot_block(snap),
            }

    def _enforce_caps_locked(self) -> None:
        while (
            sum(c.events for c in self._counters.values()) > self._max_events
            or sum(c.bytes for c in self._counters.values()) > self._max_bytes
        ):
            if not self._evict_one_locked():
                break

    def _evict_one_locked(self) -> bool:
        for lane in _EVICTION_ORDER:
            lane_deque = self._lanes[lane]
            if lane_deque:
                victim = lane_deque.popleft()
                try:
                    self._all.remove(victim)
                except ValueError:
                    pass
                counters = self._counters[lane]
                counters.events -= 1
                counters.bytes -= victim.encoded_bytes
                counters.dropped += 1
                self._dropped_total += 1
                if victim.seq + 1 > self._lost_before_seq:
                    self._lost_before_seq = victim.seq + 1
                return True
        return False

    def _pressure_locked(self) -> float:
        retained_events = sum(c.events for c in self._counters.values())
        retained_bytes = sum(c.bytes for c in self._counters.values())
        return max(
            retained_bytes / self._max_bytes,
            retained_events / self._max_events,
        )

    def _snapshot_locked(self) -> _Snapshot:
        retained_events = sum(c.events for c in self._counters.values())
        retained_bytes = sum(c.bytes for c in self._counters.values())
        pressure = max(
            retained_bytes / self._max_bytes,
            retained_events / self._max_events,
        )
        return _Snapshot(
            retained_events=retained_events,
            retained_bytes=retained_bytes,
            max_events=self._max_events,
            max_bytes=self._max_bytes,
            pressure=pressure,
            dropped_event_count=self._dropped_total,
            dropped_event_count_by_lane={
                lane: self._counters[lane].dropped for lane in _LANES
            },
            lost_before_seq=self._lost_before_seq,
            next_seq=self._next_seq,
            boot_epoch_id=self._boot_epoch_id,
        )

    @staticmethod
    def _buffer_block(snap: _Snapshot) -> dict[str, Any]:
        return {
            "retained_events": snap.retained_events,
            "retained_bytes": snap.retained_bytes,
            "max_events": snap.max_events,
            "max_bytes": snap.max_bytes,
            "pressure": snap.pressure,
            "dropped_event_count": snap.dropped_event_count,
            "dropped_event_count_by_lane": snap.dropped_event_count_by_lane,
            "lost_before_seq": snap.lost_before_seq,
        }

    @staticmethod
    def _snapshot_block(snap: _Snapshot) -> dict[str, Any]:
        return {
            "daemon": {
                "boot_epoch_id": snap.boot_epoch_id,
                "next_seq": snap.next_seq,
            },
        }


_AUDIT_BUFFER_SINGLETON: AuditBuffer | None = None
_AUDIT_BUFFER_SINGLETON_LOCK = threading.Lock()


def get_audit_buffer() -> AuditBuffer:
    """Return the process-wide singleton, creating it on first access."""
    global _AUDIT_BUFFER_SINGLETON
    with _AUDIT_BUFFER_SINGLETON_LOCK:
        if _AUDIT_BUFFER_SINGLETON is None:
            _AUDIT_BUFFER_SINGLETON = AuditBuffer()
            _wire_pressure_emitter(_AUDIT_BUFFER_SINGLETON)
        return _AUDIT_BUFFER_SINGLETON


def reset_audit_buffer_for_tests(buffer: AuditBuffer | None = None) -> AuditBuffer:
    """Replace the singleton — tests only."""
    global _AUDIT_BUFFER_SINGLETON
    with _AUDIT_BUFFER_SINGLETON_LOCK:
        _AUDIT_BUFFER_SINGLETON = buffer if buffer is not None else AuditBuffer()
        _wire_pressure_emitter(_AUDIT_BUFFER_SINGLETON)
        return _AUDIT_BUFFER_SINGLETON


def _wire_pressure_emitter(buffer: AuditBuffer) -> None:
    def _emit(snapshot_dict: dict[str, Any]) -> None:
        # Lazy import to break the audit_schema -> audit_buffer cycle.
        from sandbox.audit.schema import DaemonSection, build_daemon_event

        event = build_daemon_event(
            "daemon.audit_buffer_pressure",
            DaemonSection(
                pressure=snapshot_dict["pressure"],
                retained_events=snapshot_dict["retained_events"],
                retained_bytes=snapshot_dict["retained_bytes"],
            ),
        )
        buffer.append(event, lane="critical")

    buffer.register_pressure_cross_callback(_emit)


__all__ = [
    "AuditBuffer",
    "BufferedEvent",
    "Lane",
    "SCHEMA_VERSION",
    "get_audit_buffer",
    "reset_audit_buffer_for_tests",
]
