"""TeamRunStore — pluggable durability layer for the Dispatcher.

Two implementations ship in-tree:

* :class:`NullTeamRunStore` — default, drops every event. Preserves the
  existing in-memory-only behaviour so nothing breaks when persistence
  is disabled.
* :class:`JsonlTeamRunStore` — append-only ``events.jsonl`` per run
  under a base directory. Zero dependencies, crash-safe via ``fsync``,
  ideal for dev and tests.

Both implement the :class:`TeamRunStore` Protocol. Callers (the
Dispatcher) hold the store reference and call ``append`` while already
holding ``Dispatcher.lock`` — so ordering and atomicity come for free.

``load_run`` / ``list_runs`` support crash recovery by replaying the
event log into fresh runtime objects.
"""

from __future__ import annotations

import json
import logging
import os
import threading
from pathlib import Path
from typing import Iterable, Protocol

from team.persistence.events import TeamRunEvent

logger = logging.getLogger(__name__)


# =========================================================================
# Protocol
# =========================================================================


class TeamRunStore(Protocol):
    """Pluggable append-only event log for TeamRuns.

    Implementations must:

    * Assign a monotonic ``seq`` within a single ``team_run_id``.
    * Be safe to call while the caller holds the Dispatcher lock
      (i.e. cheap — no long blocking I/O on the hot path).
    * Survive process restart: ``load_run`` after a crash must return
      every event that ``append`` reported as complete.
    """

    def append(self, event: TeamRunEvent) -> None: ...

    def load_run(self, team_run_id: str) -> list[TeamRunEvent]: ...

    def list_runs(self) -> list[str]: ...


# =========================================================================
# NullTeamRunStore — preserves legacy in-memory-only behaviour
# =========================================================================


class NullTeamRunStore:
    """No-op store used when persistence is disabled."""

    def append(self, event: TeamRunEvent) -> None:  # noqa: D401 — Protocol impl
        return None

    def load_run(self, team_run_id: str) -> list[TeamRunEvent]:
        return []

    def list_runs(self) -> list[str]:
        return []


# =========================================================================
# JsonlTeamRunStore — append-only file per run
# =========================================================================


class JsonlTeamRunStore:
    """Append-only JSONL log, one directory per team run.

    Layout::

        <base_dir>/
          <team_run_id>/
            events.jsonl   # append-only, one JSON object per line

    Each ``append`` writes a single line and flushes + fsyncs so a kill
    -9 after ``append`` returns will not lose the event. Sequence numbers
    are assigned in-process and persisted in the event itself.
    """

    def __init__(self, base_dir: str | os.PathLike[str]) -> None:
        self._base = Path(base_dir)
        self._base.mkdir(parents=True, exist_ok=True)
        self._lock = threading.Lock()
        # Per-run sequence counters, recovered lazily on first append.
        self._seqs: dict[str, int] = {}

    # ---- path helpers ---------------------------------------------------

    def _run_dir(self, team_run_id: str) -> Path:
        return self._base / team_run_id

    def _events_path(self, team_run_id: str) -> Path:
        return self._run_dir(team_run_id) / "events.jsonl"

    # ---- sequence recovery ---------------------------------------------

    def _next_seq(self, team_run_id: str) -> int:
        if team_run_id not in self._seqs:
            # Cold start: scan the file to find the current max seq.
            path = self._events_path(team_run_id)
            last = 0
            if path.exists():
                with path.open("r", encoding="utf-8") as fh:
                    for line in fh:
                        line = line.strip()
                        if not line:
                            continue
                        try:
                            obj = json.loads(line)
                            last = max(last, int(obj.get("seq") or 0))
                        except (json.JSONDecodeError, ValueError, TypeError):
                            logger.warning("skipping malformed event line in %s", path)
            self._seqs[team_run_id] = last
        self._seqs[team_run_id] += 1
        return self._seqs[team_run_id]

    # ---- Protocol ------------------------------------------------------

    def append(self, event: TeamRunEvent) -> None:
        with self._lock:
            run_dir = self._run_dir(event.team_run_id)
            run_dir.mkdir(parents=True, exist_ok=True)
            event.seq = self._next_seq(event.team_run_id)
            path = self._events_path(event.team_run_id)
            line = json.dumps(event.to_json(), default=str, ensure_ascii=False)
            # Open per-append so we can fsync and survive kill -9. This
            # is fine at ~100s of events per run; if it becomes hot we
            # can cache the file handle per run.
            with path.open("a", encoding="utf-8") as fh:
                fh.write(line + "\n")
                fh.flush()
                os.fsync(fh.fileno())

    def load_run(self, team_run_id: str) -> list[TeamRunEvent]:
        path = self._events_path(team_run_id)
        if not path.exists():
            return []
        out: list[TeamRunEvent] = []
        with path.open("r", encoding="utf-8") as fh:
            for line in fh:
                line = line.strip()
                if not line:
                    continue
                try:
                    out.append(TeamRunEvent.from_json(json.loads(line)))
                except Exception as exc:
                    logger.warning("skipping malformed event in %s: %s", path, exc)
        out.sort(key=lambda e: e.seq)
        return out

    def list_runs(self) -> list[str]:
        if not self._base.exists():
            return []
        return sorted(
            p.name for p in self._base.iterdir()
            if p.is_dir() and (p / "events.jsonl").exists()
        )


# =========================================================================
# Replay — fold events into a simple in-memory view
# =========================================================================


def replay(events: Iterable[TeamRunEvent]) -> dict:
    """Fold an event stream into a read-only snapshot.

    This does *not* rebuild a live Dispatcher — it produces a dict
    suitable for inspection, observability dashboards, and tests. Full
    Dispatcher rehydration (re-enqueueing READY work items, re-priming
    workers) is a separate concern handled by ``TeamRun.resume_from``.
    """
    view: dict = {
        "team_run_id": None,
        "status": None,
        "work_items": {},
        "artifacts": {},
        "budget": {"tasks_used": 0, "note_bytes_used": 0},
        "checkpoints": [],
        "files": [],
    }
    for ev in events:
        view["team_run_id"] = ev.team_run_id
        if ev.kind == "team_run_created":
            view["created"] = ev.data
        elif ev.kind == "team_run_status":
            view["status"] = ev.data.get("status")
        elif ev.kind == "work_item_added":
            wi = ev.data["work_item"]
            view["work_items"][wi["id"]] = wi
        elif ev.kind == "work_item_status":
            wi = view["work_items"].get(ev.data["wi_id"])
            if wi is not None:
                wi["status"] = ev.data["status"]
                for k in ("started_at", "finished_at", "failure_reason", "agent_run_id"):
                    if k in ev.data:
                        wi[k] = ev.data[k]
        elif ev.kind == "artifact_written":
            view["artifacts"][ev.data["ref"]] = {
                "wi_id": ev.data["wi_id"],
                "size": ev.data["size"],
                "payload": ev.data["payload"],
            }
        elif ev.kind == "budget_update":
            view["budget"] = {
                "tasks_used": ev.data.get("tasks_used", ev.data.get("work_items_used", 0)),
                "note_bytes_used": ev.data.get("note_bytes_used", ev.data.get("artifact_bytes_used", 0)),
                "replans_used": ev.data.get("replans_used", 0),
            }
        elif ev.kind == "checkpoint_taken":
            view["checkpoints"].append(ev.data)
        elif ev.kind == "file_changed":
            view["files"].append(ev.data)
    return view


# =========================================================================
# Factory
# =========================================================================


def build_default_store(
    *,
    base_dir: str | os.PathLike[str] | None = None,
    session_factory: object | None = None,
) -> TeamRunStore:
    """Pick a sensible default store.

    Precedence:

    1. ``base_dir`` provided or ``EPHEMERALOS_TEAM_RUN_DIR`` set →
       ``JsonlTeamRunStore``.
    2. Neither → ``NullTeamRunStore`` (in-memory only).

    The ``session_factory`` parameter is accepted but ignored (SQL store
    removed in favour of JSONL).
    """
    env_dir = os.environ.get("EPHEMERALOS_TEAM_RUN_DIR")
    chosen = base_dir or env_dir
    if chosen:
        return JsonlTeamRunStore(chosen)
    return NullTeamRunStore()
