"""TeamRunStore — append-only TeamRun event log for the TaskCenter.

When configured with a base directory, events are persisted as
``events.jsonl`` under a directory named after the TeamRun id. Without a base
directory, the store is disabled and drops events.
"""

from __future__ import annotations

import json
import logging
import os
import threading
from pathlib import Path

from team.persistence.events import TeamRunEvent

logger = logging.getLogger(__name__)


class TeamRunStore:
    """Append-only TeamRun event log.

    The store writes one ``events.jsonl`` file per TeamRun when ``base_dir`` is
    configured. When ``base_dir`` is ``None``, the store acts as a disabled
    sink so callers do not need a second no-op implementation.
    """

    def __init__(self, base_dir: str | os.PathLike[str] | None = None) -> None:
        self._base = Path(base_dir) if base_dir is not None else None
        if self._base is not None:
            self._base.mkdir(parents=True, exist_ok=True)
        self._lock = threading.Lock()
        self._seqs: dict[str, int] = {}

    def _run_dir(self, team_run_id: str) -> Path:
        if self._base is None:
            raise RuntimeError("TeamRunStore is disabled; no base directory configured")
        return self._base / team_run_id

    def _events_path(self, team_run_id: str) -> Path:
        return self._run_dir(team_run_id) / "events.jsonl"

    def _next_seq(self, team_run_id: str) -> int:
        if team_run_id not in self._seqs:
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

    def append(self, event: TeamRunEvent) -> None:
        if self._base is None:
            return None
        with self._lock:
            run_dir = self._run_dir(event.team_run_id)
            run_dir.mkdir(parents=True, exist_ok=True)
            event.seq = self._next_seq(event.team_run_id)
            path = self._events_path(event.team_run_id)
            line = json.dumps(event.to_json(), default=str, ensure_ascii=False)
            with path.open("a", encoding="utf-8") as fh:
                fh.write(line + "\n")
                fh.flush()
                os.fsync(fh.fileno())

    def load_run(self, team_run_id: str) -> list[TeamRunEvent]:
        if self._base is None:
            return []
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
        if self._base is None or not self._base.exists():
            return []
        return sorted(
            p.name for p in self._base.iterdir()
            if p.is_dir() and (p / "events.jsonl").exists()
        )


def build_default_store(
    *, base_dir: str | os.PathLike[str] | None = None,
    session_factory: object | None = None,
) -> TeamRunStore:
    env_dir = os.environ.get("EPHEMERALOS_TEAM_RUN_DIR")
    chosen = base_dir or env_dir
    return TeamRunStore(chosen)
