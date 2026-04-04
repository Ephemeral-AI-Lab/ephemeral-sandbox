"""Arbiter — Optimistic Concurrency Control for file edits.

Provides per-file write coordination to prevent conflicts when multiple
agents edit the same file. Uses edit tokens with TTL for staleness detection.

Lock ordering (Group A):
    Arbiter locks < Cache locks < Counter locks
"""

from __future__ import annotations

import hashlib
import logging
import threading
import time
import uuid
from dataclasses import dataclass, field
from typing import Any, Callable

from ephemeralos.services.code_intelligence.constants import (
    ARBITER_LOCK_TIMEOUT,
    ARBITER_MAX_CONCURRENT_EDITS,
)

logger = logging.getLogger(__name__)

_EDIT_TOKEN_TTL = 300.0  # 5 minutes


def _content_hash(content: str) -> str:
    return hashlib.sha256(content.encode("utf-8")).hexdigest()[:16]


@dataclass
class EditToken:
    """Token issued when a file is read-for-edit."""

    token_id: str
    file_path: str
    content_hash: str
    issued_at: float
    agent_id: str = ""
    ttl: float = _EDIT_TOKEN_TTL


@dataclass
class ArbiterMetrics:
    """Edit coordination metrics."""

    total_edits: int = 0
    conflicts_detected: int = 0
    tokens_issued: int = 0
    tokens_expired: int = 0
    active_locks: int = 0


class Arbiter:
    """Per-file edit arbitration with OCC.

    Thread-safe. Uses per-file locks to serialize edits to the same file
    while allowing concurrent edits to different files.

    Parameters
    ----------
    workspace_root:
        Root directory for path validation.
    on_edit:
        Optional callback ``(file_path, agent_id, generation)`` after successful edit.
    max_concurrent:
        Maximum concurrent file edits.
    """

    def __init__(
        self,
        workspace_root: str = "",
        on_edit: Callable[[str, str, int], None] | None = None,
        max_concurrent: int = ARBITER_MAX_CONCURRENT_EDITS,
    ) -> None:
        self._workspace_root = workspace_root
        self._on_edit = on_edit
        self._max_concurrent = max_concurrent

        self._lock = threading.Lock()
        self._file_locks: dict[str, threading.Lock] = {}
        self._active_tokens: dict[str, EditToken] = {}  # token_id -> token
        self._metrics = ArbiterMetrics()
        self._generation = 0
        # Hotspot tracking: file_path -> edit count
        self._hotspots: dict[str, int] = {}

    # -- Token management -----------------------------------------------------

    def issue_token(
        self, file_path: str, content_hash: str, agent_id: str = "",
    ) -> EditToken:
        """Issue an edit token for a file."""
        token = EditToken(
            token_id=uuid.uuid4().hex[:12],
            file_path=file_path,
            content_hash=content_hash,
            issued_at=time.time(),
            agent_id=agent_id,
        )
        with self._lock:
            self._active_tokens[token.token_id] = token
            self._metrics.tokens_issued += 1
        return token

    def validate_token(self, token_id: str, current_hash: str) -> tuple[bool, str]:
        """Validate an edit token. Returns (valid, reason)."""
        with self._lock:
            token = self._active_tokens.get(token_id)

        if token is None:
            return False, "Token not found or already consumed"

        if time.time() - token.issued_at > token.ttl:
            with self._lock:
                self._active_tokens.pop(token_id, None)
                self._metrics.tokens_expired += 1
            return False, "Token expired"

        if token.content_hash != current_hash:
            with self._lock:
                self._metrics.conflicts_detected += 1
            return False, (
                f"Content changed since token was issued "
                f"(expected {token.content_hash}, got {current_hash})"
            )

        return True, "ok"

    def consume_token(self, token_id: str) -> EditToken | None:
        """Consume (remove) a token after successful edit."""
        with self._lock:
            return self._active_tokens.pop(token_id, None)

    def refresh_token(
        self, token_id: str, new_hash: str,
    ) -> EditToken | None:
        """Refresh a token with updated content hash (for retry after conflict)."""
        with self._lock:
            old = self._active_tokens.pop(token_id, None)
        if old is None:
            return None
        return self.issue_token(old.file_path, new_hash, old.agent_id)

    # -- Edit coordination ----------------------------------------------------

    def acquire_file_lock(
        self, file_path: str, timeout: float = ARBITER_LOCK_TIMEOUT,
    ) -> bool:
        """Acquire the per-file edit lock. Returns True if acquired."""
        lock = self._get_file_lock(file_path)
        return lock.acquire(timeout=timeout)

    def release_file_lock(self, file_path: str) -> None:
        """Release the per-file edit lock."""
        lock = self._get_file_lock(file_path)
        try:
            lock.release()
        except RuntimeError:
            pass  # Already released

    def record_edit(self, file_path: str, agent_id: str = "") -> int:
        """Record a successful edit. Returns the new generation."""
        with self._lock:
            self._generation += 1
            gen = self._generation
            self._metrics.total_edits += 1
            self._hotspots[file_path] = self._hotspots.get(file_path, 0) + 1

        if self._on_edit:
            try:
                self._on_edit(file_path, agent_id, gen)
            except Exception:
                logger.debug("on_edit callback failed for %s", file_path)

        return gen

    # -- Queries --------------------------------------------------------------

    def hotspots(self, limit: int = 10) -> list[tuple[str, int]]:
        """Return the most frequently edited files."""
        with self._lock:
            sorted_files = sorted(
                self._hotspots.items(), key=lambda x: x[1], reverse=True,
            )
            return sorted_files[:limit]

    @property
    def metrics(self) -> ArbiterMetrics:
        with self._lock:
            return ArbiterMetrics(
                total_edits=self._metrics.total_edits,
                conflicts_detected=self._metrics.conflicts_detected,
                tokens_issued=self._metrics.tokens_issued,
                tokens_expired=self._metrics.tokens_expired,
                active_locks=len(self._file_locks),
            )

    @property
    def active_edit_count(self) -> int:
        with self._lock:
            return len(self._active_tokens)

    def status(self) -> dict[str, Any]:
        """Return arbiter status summary."""
        m = self.metrics
        return {
            "total_edits": m.total_edits,
            "conflicts_detected": m.conflicts_detected,
            "tokens_issued": m.tokens_issued,
            "tokens_expired": m.tokens_expired,
            "active_tokens": self.active_edit_count,
            "active_locks": m.active_locks,
        }

    def cleanup_locks(self) -> int:
        """Remove file locks that are not held. Returns count cleaned."""
        with self._lock:
            to_remove = [
                fp for fp, lock in self._file_locks.items()
                if not lock.locked()
            ]
            for fp in to_remove:
                del self._file_locks[fp]
            return len(to_remove)

    # -- Internal -------------------------------------------------------------

    def _get_file_lock(self, file_path: str) -> threading.Lock:
        with self._lock:
            if file_path not in self._file_locks:
                self._file_locks[file_path] = threading.Lock()
            return self._file_locks[file_path]
