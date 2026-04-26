"""TimeMachine — per-file undo snapshots with global LRU capacity.

Provides rollback within a session. Each file maintains a small stack
of snapshots (default max 5 per file, 50 MB global).
"""

from __future__ import annotations

import logging
import threading
import time
import uuid
from collections import OrderedDict
from dataclasses import dataclass

from code_intelligence.core.hashing import content_hash as _content_hash

logger = logging.getLogger(__name__)

_MAX_SNAPSHOTS_PER_FILE = 5
_MAX_GLOBAL_BYTES = 50 * 1024 * 1024  # 50 MB


@dataclass(frozen=True)
class SnapshotEntry:
    """A point-in-time file snapshot."""

    snapshot_id: str
    content: str
    content_hash: str
    timestamp: float
    existed: bool = True


class TimeMachine:
    """Per-file snapshot stacks with global LRU eviction.

    Thread-safe standalone lock (Group D).
    """

    def __init__(
        self,
        max_per_file: int = _MAX_SNAPSHOTS_PER_FILE,
        max_global_bytes: int = _MAX_GLOBAL_BYTES,
    ) -> None:
        self._max_per_file = max_per_file
        self._max_global_bytes = max_global_bytes
        self._lock = threading.Lock()
        # file_path -> stack of snapshots (most recent last)
        self._stacks: OrderedDict[str, list[SnapshotEntry]] = OrderedDict()
        self._total_bytes = 0

    def save(self, file_path: str, content: str, *, existed: bool = True) -> str:
        """Save a snapshot before editing. Returns the snapshot_id."""
        snapshot = SnapshotEntry(
            snapshot_id=uuid.uuid4().hex[:12],
            content=content,
            content_hash=_content_hash(content),
            timestamp=time.time(),
            existed=existed,
        )
        content_size = len(content.encode("utf-8"))

        with self._lock:
            stack = self._stacks.get(file_path, [])

            # Trim per-file stack
            while len(stack) >= self._max_per_file:
                removed = stack.pop(0)
                self._total_bytes -= len(removed.content.encode("utf-8"))

            stack.append(snapshot)
            self._stacks[file_path] = stack
            self._stacks.move_to_end(file_path)
            self._total_bytes += content_size

            # Global capacity eviction (LRU)
            while self._total_bytes > self._max_global_bytes and self._stacks:
                oldest_path, oldest_stack = next(iter(self._stacks.items()))
                if oldest_stack:
                    removed = oldest_stack.pop(0)
                    self._total_bytes -= len(removed.content.encode("utf-8"))
                if not oldest_stack:
                    del self._stacks[oldest_path]

        return snapshot.snapshot_id

    def rollback(self, file_path: str) -> SnapshotEntry | None:
        """Pop and return the most recent snapshot for rollback."""
        with self._lock:
            stack = self._stacks.get(file_path, [])
            if not stack:
                return None
            snapshot = stack.pop()
            self._total_bytes -= len(snapshot.content.encode("utf-8"))
            if not stack:
                del self._stacks[file_path]
            return snapshot

    def clear(self, file_path: str | None = None) -> None:
        """Clear snapshots for a file, or all files if None."""
        with self._lock:
            if file_path:
                stack = self._stacks.pop(file_path, [])
                for s in stack:
                    self._total_bytes -= len(s.content.encode("utf-8"))
            else:
                self._stacks.clear()
                self._total_bytes = 0
