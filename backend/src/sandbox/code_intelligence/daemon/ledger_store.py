"""SQLite-backed edit-history ledger for the in-sandbox CI daemon."""

from __future__ import annotations

import logging
import sqlite3
import threading
import time
from collections import Counter
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Any

logger = logging.getLogger(__name__)

_LEDGER_FILE = "ledger.sqlite3"

_LEDGER_SCHEMA_SQL = """
CREATE TABLE IF NOT EXISTS edits (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    ts REAL NOT NULL,
    run_id TEXT NOT NULL DEFAULT '',
    agent_run_id TEXT NOT NULL DEFAULT '',
    task_id TEXT NOT NULL DEFAULT '',
    agent_id TEXT NOT NULL DEFAULT '',
    file_path TEXT NOT NULL,
    edit_type TEXT NOT NULL DEFAULT 'edit',
    old_hash TEXT NOT NULL DEFAULT '',
    new_hash TEXT NOT NULL DEFAULT '',
    description TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_edits_file ON edits(file_path);
CREATE INDEX IF NOT EXISTS idx_edits_ts ON edits(ts);
CREATE INDEX IF NOT EXISTS idx_edits_run ON edits(run_id);
CREATE INDEX IF NOT EXISTS idx_edits_agent_run ON edits(run_id, agent_run_id);
"""


def _apply_ledger_pragmas(conn: sqlite3.Connection) -> None:
    conn.execute("PRAGMA journal_mode = WAL;")
    conn.execute("PRAGMA synchronous = NORMAL;")
    conn.execute("PRAGMA temp_store = MEMORY;")
    conn.execute("PRAGMA mmap_size = 67108864;")


def _open_ledger_db(path: Path) -> sqlite3.Connection:
    """Open ``path`` with WAL pragmas, rotating the DB on integrity failure."""
    path.parent.mkdir(parents=True, exist_ok=True)
    conn = sqlite3.connect(path, isolation_level=None, check_same_thread=False)
    integrity: str = "ok"
    try:
        _apply_ledger_pragmas(conn)
        row = conn.execute("PRAGMA integrity_check").fetchone()
        integrity = row[0] if row else "unknown"
    except sqlite3.DatabaseError as exc:
        integrity = str(exc) or "database error"
    if integrity != "ok":
        logger.warning(
            "storage: ledger %s failed integrity check (%s); rotating",
            path,
            integrity,
        )
        try:
            conn.close()
        except sqlite3.Error:
            pass
        rotated = path.with_suffix(f".corrupt.{int(time.time())}.sqlite3")
        try:
            path.rename(rotated)
        except OSError:
            try:
                path.unlink()
            except OSError:
                pass
        conn = sqlite3.connect(path, isolation_level=None, check_same_thread=False)
        _apply_ledger_pragmas(conn)
    conn.executescript(_LEDGER_SCHEMA_SQL)
    return conn


class LedgerStore:
    """SQLite-WAL backed implementation of the EditHistoryLedger interface."""

    initialized: bool = True

    def __init__(self, state_dir_path: Path) -> None:
        self._path = state_dir_path / _LEDGER_FILE
        self._lock = threading.Lock()
        self._conn = _open_ledger_db(self._path)

    @property
    def path(self) -> Path:
        return self._path

    def close(self) -> None:
        with self._lock:
            try:
                self._conn.close()
            except sqlite3.Error:
                pass

    def _row_to_record(self, row: sqlite3.Row | tuple) -> Any:
        from sandbox.code_intelligence.mutations.edit_history_ledger import (
            EditRecord,
        )

        (
            seq,
            ts,
            run_id,
            agent_run_id,
            task_id,
            _agent_id,
            file_path,
            edit_type,
            old_hash,
            new_hash,
            description,
        ) = row
        return EditRecord(
            id=int(seq),
            file_path=str(file_path),
            run_id=str(run_id or ""),
            agent_run_id=str(agent_run_id or ""),
            task_id=str(task_id or ""),
            edit_type=str(edit_type or "edit"),
            old_hash=str(old_hash or ""),
            new_hash=str(new_hash or ""),
            description=str(description or ""),
            created_at=datetime.fromtimestamp(float(ts), tz=timezone.utc),
        )

    def record(
        self,
        *,
        run_id: str,
        file_path: str,
        agent_run_id: str = "",
        task_id: str = "",
        edit_type: str = "edit",
        old_hash: str = "",
        new_hash: str = "",
        description: str = "",
    ) -> Any:
        """Persist one edit row and return the resulting EditRecord."""
        ts = time.time()
        with self._lock:
            cur = self._conn.execute(
                "INSERT INTO edits "
                "(ts, run_id, agent_run_id, task_id, agent_id, file_path, "
                " edit_type, old_hash, new_hash, description) "
                "VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                (
                    ts,
                    run_id or "",
                    agent_run_id or "",
                    task_id or "",
                    "",
                    file_path,
                    edit_type or "edit",
                    old_hash or "",
                    new_hash or "",
                    description or "",
                ),
            )
            seq = int(cur.lastrowid or 0)
        from sandbox.code_intelligence.mutations.edit_history_ledger import (
            EditRecord,
        )

        return EditRecord(
            id=seq,
            file_path=file_path,
            run_id=run_id or "",
            agent_run_id=agent_run_id or "",
            task_id=task_id or "",
            edit_type=edit_type or "edit",
            old_hash=old_hash or "",
            new_hash=new_hash or "",
            description=description or "",
            created_at=datetime.fromtimestamp(ts, tz=timezone.utc),
        )

    def _select_rows(self, sql: str, params: tuple = ()) -> list[Any]:
        with self._lock:
            cursor = self._conn.execute(sql, params)
            rows = cursor.fetchall()
        return [self._row_to_record(row) for row in rows]

    def changes_in_scope(
        self,
        run_id: str,
        scope_prefixes: list[str],
        since: float,
    ) -> list[Any]:
        normalized = [p.rstrip("/") for p in scope_prefixes if p]
        if not normalized:
            return []
        like_clauses = " OR ".join(["file_path LIKE ?"] * len(normalized))
        params = [run_id, float(since), *(f"{prefix}%" for prefix in normalized)]
        sql = (
            "SELECT seq, ts, run_id, agent_run_id, task_id, agent_id, file_path, "
            "edit_type, old_hash, new_hash, description "
            "FROM edits "
            f"WHERE run_id = ? AND ts > ? AND ({like_clauses}) "
            "ORDER BY seq"
        )
        return self._select_rows(sql, tuple(params))

    def external_changes_in_scope(
        self,
        run_id: str,
        scope_prefixes: list[str],
        since: float,
        exclude_run_id: str | None = None,
    ) -> list[Any]:
        rows = self.changes_in_scope(run_id, scope_prefixes, since)
        if exclude_run_id:
            rows = [r for r in rows if r.agent_run_id != exclude_run_id]
        return rows

    def changes_since(
        self,
        since: float,
        run_id: str | None = None,
    ) -> list[Any]:
        if run_id is None:
            sql = (
                "SELECT seq, ts, run_id, agent_run_id, task_id, agent_id, file_path, "
                "edit_type, old_hash, new_hash, description "
                "FROM edits WHERE ts > ? ORDER BY seq"
            )
            return self._select_rows(sql, (float(since),))
        sql = (
            "SELECT seq, ts, run_id, agent_run_id, task_id, agent_id, file_path, "
            "edit_type, old_hash, new_hash, description "
            "FROM edits WHERE ts > ? AND run_id = ? ORDER BY seq"
        )
        return self._select_rows(sql, (float(since), run_id))

    def recent_edits(
        self,
        seconds: float = 60.0,
        run_id: str | None = None,
    ) -> list[Any]:
        return self.changes_since(time.time() - seconds, run_id=run_id)

    def hotspots(
        self,
        limit: int = 10,
        run_id: str | None = None,
    ) -> list[tuple[str, int]]:
        if run_id is None:
            sql = "SELECT file_path FROM edits"
            params: tuple = ()
        else:
            sql = "SELECT file_path FROM edits WHERE run_id = ?"
            params = (run_id,)
        with self._lock:
            rows = self._conn.execute(sql, params).fetchall()
        counter: Counter[str] = Counter(str(r[0]) for r in rows)
        return counter.most_common(limit)

    def who_changed(
        self,
        file_path: str,
        run_id: str | None = None,
    ) -> list[Any]:
        sql = (
            "SELECT seq, ts, run_id, agent_run_id, task_id, agent_id, file_path, "
            "edit_type, old_hash, new_hash, description "
            "FROM edits WHERE file_path = ?"
        )
        params: tuple[Any, ...] = (file_path,)
        if run_id is not None:
            sql += " AND run_id = ?"
            params = (file_path, run_id)
        return self._select_rows(sql + " ORDER BY seq", params)

    def changes_by_agent_run(
        self,
        run_id: str,
        agent_run_id: str,
    ) -> list[Any]:
        if not agent_run_id:
            return []
        sql = (
            "SELECT seq, ts, run_id, agent_run_id, task_id, agent_id, file_path, "
            "edit_type, old_hash, new_hash, description "
            "FROM edits WHERE run_id = ? AND agent_run_id = ? ORDER BY seq"
        )
        return self._select_rows(sql, (run_id, agent_run_id))

    def contention_hotspots(
        self,
        scope_prefixes: list[str] | None = None,
        limit: int = 10,
        days: int = 7,
        run_id: str | None = None,
    ) -> list[Any]:
        from sandbox.code_intelligence.mutations.edit_history_ledger import (
            ContentionHotspot,
        )

        cutoff = (datetime.now(timezone.utc) - timedelta(days=days)).timestamp()
        clauses = ["ts > ?"]
        params: list[Any] = [cutoff]
        if run_id is not None:
            clauses.append("run_id = ?")
            params.append(run_id)
        normalized = [p.rstrip("/") for p in (scope_prefixes or []) if p]
        if normalized:
            like_clauses = " OR ".join(["file_path LIKE ?"] * len(normalized))
            clauses.append(f"({like_clauses})")
            params.extend(f"{prefix}%" for prefix in normalized)
        sql = (
            "SELECT seq, ts, run_id, agent_run_id, task_id, agent_id, file_path, "
            "edit_type, old_hash, new_hash, description "
            f"FROM edits WHERE {' AND '.join(clauses)} ORDER BY seq"
        )
        records = self._select_rows(sql, tuple(params))

        contributors_by_file: dict[str, set[str]] = {}
        counts: Counter[str] = Counter()
        for r in records:
            contributor = r.task_id or r.agent_run_id or f"edit:{r.id}"
            contributors_by_file.setdefault(r.file_path, set()).add(contributor)
            counts[r.file_path] += 1
        results = [
            ContentionHotspot(
                file_path=fp,
                contributor_count=len(contributors),
                edit_count=counts[fp],
            )
            for fp, contributors in contributors_by_file.items()
            if len(contributors) > 1
        ]
        results.sort(key=lambda h: (-h.contributor_count, -h.edit_count))
        return results[:limit]
