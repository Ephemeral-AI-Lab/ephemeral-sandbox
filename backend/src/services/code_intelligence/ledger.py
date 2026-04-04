"""Ledger — bounded edit audit log with O(1) filepath lookup.

Append-only ring buffer that records agent-attributed edits. Supports
O(log n) ``changes_since()`` via parallel timestamp deque and bisect,
and O(1) ``who_changed()`` via filepath index.
"""

from __future__ import annotations

import bisect
import logging
import threading
import time
from collections import defaultdict, deque
from dataclasses import dataclass, field

from ephemeralos.services.code_intelligence.constants import LEDGER_MAX_ENTRIES

logger = logging.getLogger(__name__)


@dataclass(frozen=True)
class LedgerEntry:
    """Immutable edit log entry."""

    file_path: str
    agent_id: str
    timestamp: float
    edit_type: str = "edit"  # edit, create, delete, shell_mutation
    old_hash: str = ""
    new_hash: str = ""
    description: str = ""


class Ledger:
    """Bounded edit audit log.

    Thread-safe standalone lock (Group D — leaf lock, no ordering constraints).

    Parameters
    ----------
    max_entries:
        Ring buffer capacity. Oldest entries are evicted first.
    """

    def __init__(self, max_entries: int = LEDGER_MAX_ENTRIES) -> None:
        self._max_entries = max_entries
        self._lock = threading.Lock()
        self._entries: deque[LedgerEntry] = deque(maxlen=max_entries)
        self._timestamps: deque[float] = deque(maxlen=max_entries)
        self._by_file: dict[str, list[LedgerEntry]] = defaultdict(list)
        self._ref_counts: dict[str, int] = defaultdict(int)

    def record(
        self,
        file_path: str,
        agent_id: str,
        edit_type: str = "edit",
        old_hash: str = "",
        new_hash: str = "",
        description: str = "",
    ) -> LedgerEntry:
        """Record an edit. Returns the new entry."""
        entry = LedgerEntry(
            file_path=file_path,
            agent_id=agent_id,
            timestamp=time.time(),
            edit_type=edit_type,
            old_hash=old_hash,
            new_hash=new_hash,
            description=description,
        )
        with self._lock:
            # Evict if at capacity
            if len(self._entries) >= self._max_entries:
                evicted = self._entries[0]
                self._ref_counts[evicted.file_path] -= 1
                if self._ref_counts[evicted.file_path] <= 0:
                    del self._ref_counts[evicted.file_path]
                    self._by_file.pop(evicted.file_path, None)
                else:
                    # Remove the oldest entry from the file's list
                    file_entries = self._by_file[evicted.file_path]
                    if file_entries and file_entries[0] is evicted:
                        file_entries.pop(0)

            self._entries.append(entry)
            self._timestamps.append(entry.timestamp)
            self._by_file[file_path].append(entry)
            self._ref_counts[file_path] += 1

        return entry

    def changes_since(self, since: float) -> list[LedgerEntry]:
        """Return all entries after *since* timestamp. O(log n) via bisect."""
        with self._lock:
            idx = bisect.bisect_right(list(self._timestamps), since)
            return list(self._entries)[idx:]

    def who_changed(self, file_path: str) -> list[LedgerEntry]:
        """Return all entries for a file. O(1) lookup."""
        with self._lock:
            return list(self._by_file.get(file_path, []))

    def recent_files(self, seconds: float = 60.0) -> list[str]:
        """Return deduplicated list of files changed in the last N seconds."""
        cutoff = time.time() - seconds
        entries = self.changes_since(cutoff)
        seen: set[str] = set()
        result: list[str] = []
        for e in entries:
            if e.file_path not in seen:
                seen.add(e.file_path)
                result.append(e.file_path)
        return result

    def to_dicts(self) -> list[dict]:
        """Serialize all entries for persistence."""
        with self._lock:
            return [
                {
                    "file_path": e.file_path,
                    "agent_id": e.agent_id,
                    "timestamp": e.timestamp,
                    "edit_type": e.edit_type,
                    "old_hash": e.old_hash,
                    "new_hash": e.new_hash,
                    "description": e.description,
                }
                for e in self._entries
            ]

    def restore_entries(self, entries: list[dict]) -> None:
        """Restore from persisted data."""
        with self._lock:
            self._entries.clear()
            self._timestamps.clear()
            self._by_file.clear()
            self._ref_counts.clear()
            for d in entries:
                entry = LedgerEntry(**d)
                self._entries.append(entry)
                self._timestamps.append(entry.timestamp)
                self._by_file[entry.file_path].append(entry)
                self._ref_counts[entry.file_path] += 1

    @property
    def entry_count(self) -> int:
        with self._lock:
            return len(self._entries)

    def clear(self) -> None:
        """Clear all entries."""
        with self._lock:
            self._entries.clear()
            self._timestamps.clear()
            self._by_file.clear()
            self._ref_counts.clear()
