"""SQLite-backed symbol-index storage for the in-sandbox CI daemon."""

from __future__ import annotations

import logging
import sqlite3
import time
import threading
from pathlib import Path
from typing import Any

from sandbox.code_intelligence.daemon.paths import _read_pickle_snapshot

logger = logging.getLogger(__name__)

_INDEX_FILE = "index.sqlite3"

_INDEX_SCHEMA_SQL = """
CREATE TABLE IF NOT EXISTS index_files (
    file_path TEXT PRIMARY KEY,
    generation INTEGER NOT NULL DEFAULT 0,
    indexed_at REAL NOT NULL,
    symbols_blob BLOB NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_index_files_generation ON index_files(generation);
"""


def _apply_index_pragmas(conn: sqlite3.Connection) -> None:
    conn.execute("PRAGMA journal_mode = WAL;")
    conn.execute("PRAGMA synchronous = NORMAL;")
    conn.execute("PRAGMA temp_store = MEMORY;")
    conn.execute("PRAGMA mmap_size = 67108864;")


def _open_index_db(path: Path) -> sqlite3.Connection:
    """Open ``path`` with WAL pragmas; integrity-check + rotate on failure."""
    path.parent.mkdir(parents=True, exist_ok=True)
    conn = sqlite3.connect(path, isolation_level=None, check_same_thread=False)
    integrity: str = "ok"
    try:
        _apply_index_pragmas(conn)
        row = conn.execute("PRAGMA integrity_check").fetchone()
        integrity = row[0] if row else "unknown"
    except sqlite3.DatabaseError as exc:
        integrity = str(exc) or "database error"
    if integrity != "ok":
        logger.warning(
            "storage: index %s failed integrity check (%s); rotating",
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
        _apply_index_pragmas(conn)
    conn.executescript(_INDEX_SCHEMA_SQL)
    return conn


def _encode_symbols(symbols: list[Any]) -> bytes:
    """msgpack-encode a list of SymbolInfo dataclasses to a single blob."""
    import msgpack

    payload = []
    for sym in symbols:
        kind = getattr(sym, "kind", "")
        payload.append(
            {
                "name": str(getattr(sym, "name", "")),
                "kind": str(getattr(kind, "value", kind)),
                "file_path": str(getattr(sym, "file_path", "")),
                "line": int(getattr(sym, "line", 0)),
                "end_line": getattr(sym, "end_line", None),
                "character": int(getattr(sym, "character", 0)),
                "signature": str(getattr(sym, "signature", "")),
                "docstring": str(getattr(sym, "docstring", "")),
                "container": str(getattr(sym, "container", "")),
            }
        )
    return msgpack.packb(payload, use_bin_type=True)


def _decode_symbols(blob: bytes) -> list[Any]:
    """msgpack-decode a blob back into SymbolInfo dataclasses."""
    import msgpack

    from sandbox.code_intelligence.core.types import SymbolInfo, SymbolKind

    if not blob:
        return []
    payload = msgpack.unpackb(blob, raw=False)
    out: list[Any] = []
    for d in payload:
        kind_raw = d.get("kind", "unknown")
        try:
            kind = SymbolKind(kind_raw)
        except ValueError:
            kind = SymbolKind.UNKNOWN
        out.append(
            SymbolInfo(
                name=str(d.get("name", "")),
                kind=kind,
                file_path=str(d.get("file_path", "")),
                line=int(d.get("line", 0)),
                end_line=d.get("end_line"),
                character=int(d.get("character", 0)),
                signature=str(d.get("signature", "")),
                docstring=str(d.get("docstring", "")),
                container=str(d.get("container", "")),
            )
        )
    return out


class IndexStore:
    """SQLite-WAL backed symbol-index storage."""

    def __init__(self, state_dir_path: Path) -> None:
        self._path = state_dir_path / _INDEX_FILE
        self._lock = threading.Lock()
        self._conn = _open_index_db(self._path)
        self._generation = self._read_max_generation()

    @property
    def path(self) -> Path:
        return self._path

    @property
    def generation(self) -> int:
        with self._lock:
            return self._generation

    def close(self) -> None:
        with self._lock:
            try:
                self._conn.close()
            except sqlite3.Error:
                pass

    def _read_max_generation(self) -> int:
        with self._lock:
            row = self._conn.execute(
                "SELECT COALESCE(MAX(generation), 0) FROM index_files"
            ).fetchone()
            return int(row[0] if row else 0)

    def bulk_replace(self, snapshot: dict[str, list[Any]]) -> int:
        """Atomic full replacement. Returns the new generation."""
        now = time.time()
        with self._lock:
            self._generation += 1
            gen = self._generation
            try:
                self._conn.execute("BEGIN IMMEDIATE")
                self._conn.execute("DELETE FROM index_files")
                self._conn.executemany(
                    "INSERT INTO index_files "
                    "(file_path, generation, indexed_at, symbols_blob) "
                    "VALUES (?, ?, ?, ?)",
                    [
                        (str(fp), gen, now, _encode_symbols(syms))
                        for fp, syms in snapshot.items()
                    ],
                )
                self._conn.execute("COMMIT")
            except sqlite3.Error:
                try:
                    self._conn.execute("ROLLBACK")
                except sqlite3.Error:
                    pass
                raise
        return gen

    def refresh_file(self, file_path: str, symbols: list[Any]) -> int:
        """INSERT OR REPLACE one row; returns the new generation."""
        now = time.time()
        blob = _encode_symbols(symbols)
        with self._lock:
            self._generation += 1
            gen = self._generation
            self._conn.execute(
                "INSERT OR REPLACE INTO index_files "
                "(file_path, generation, indexed_at, symbols_blob) "
                "VALUES (?, ?, ?, ?)",
                (str(file_path), gen, now, blob),
            )
        return gen

    def delete_file(self, file_path: str) -> int:
        """DELETE one row; returns the new generation."""
        with self._lock:
            self._generation += 1
            gen = self._generation
            self._conn.execute(
                "DELETE FROM index_files WHERE file_path = ?",
                (str(file_path),),
            )
        return gen

    def file_symbols(self, file_path: str) -> list[Any]:
        """PK lookup for a single file's symbols."""
        with self._lock:
            row = self._conn.execute(
                "SELECT symbols_blob FROM index_files WHERE file_path = ?",
                (str(file_path),),
            ).fetchone()
        if not row:
            return []
        return _decode_symbols(row[0])

    def indexed_paths(self) -> list[str]:
        """All indexed file paths, sorted."""
        with self._lock:
            rows = self._conn.execute(
                "SELECT file_path FROM index_files ORDER BY file_path"
            ).fetchall()
        return [str(r[0]) for r in rows]


def migrate_pickle_to_sqlite(state: Path) -> int:
    """One-shot pickle ``index.snapshot`` to SQLite ``index.sqlite3`` migration."""
    pickle_path = state / "index.snapshot"
    if not pickle_path.exists():
        return 0
    snapshot = _read_pickle_snapshot(state, "index.snapshot")
    if not isinstance(snapshot, dict) or not snapshot:
        try:
            pickle_path.unlink()
        except OSError:
            pass
        return 0
    store = IndexStore(state_dir_path=state)
    try:
        store.bulk_replace(snapshot)
    finally:
        store.close()
    try:
        pickle_path.unlink()
    except OSError:
        pass
    logger.info("storage: migrated %d files from pickle to sqlite", len(snapshot))
    return len(snapshot)
