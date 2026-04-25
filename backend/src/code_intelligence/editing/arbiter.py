"""Arbiter — edit audit ledger and lightweight file coordination.

Records audited edit operations and exposes coordination metadata for
code-intelligence consumers. Daytona mutation tools record process-level
changes through ``CodeIntelligenceService.cmd`` (OCC-gated Git workspace audit).

Queryable edit history is delegated to an internal EditHistoryLedger so
callers can depend on Arbiter as the public coordination facade.

Lock ordering (Group A):
    Arbiter locks < Cache locks < Counter locks
"""

from __future__ import annotations

import logging
import threading
from collections.abc import Callable
from dataclasses import dataclass
from typing import Any

from code_intelligence.constants import (
    ARBITER_LOCK_TIMEOUT,
    ARBITER_MAX_CONCURRENT_EDITS,
)
from code_intelligence.editing.edit_history_ledger import EditHistoryLedger

logger = logging.getLogger(__name__)


@dataclass
class ArbiterMetrics:
    """Edit coordination metrics."""

    total_edits: int = 0
    conflicts_detected: int = 0
    active_locks: int = 0


class Arbiter:
    """Per-sandbox edit ledger and optional file arbitration.

    Thread-safe. Uses per-file locks to serialize edits to the same file
    while allowing concurrent edits to different files.

    Edit history is delegated to EditHistoryLedger. The Arbiter also owns
    lightweight per-file locks for semantic service helpers.

    Parameters
    ----------
    workspace_root:
        Root directory for path validation.
    on_edit:
        Optional callback ``(file_path, actor_label, generation)`` after successful edit.
    edit_history:
        Queryable edit-history ledger used by coordination readers.
    max_concurrent:
        Maximum concurrent file edits.
    """

    def __init__(
        self,
        workspace_root: str = "",
        on_edit: Callable[[str, str, int], None] | None = None,
        edit_history: EditHistoryLedger | None = None,
        max_concurrent: int = ARBITER_MAX_CONCURRENT_EDITS,
    ) -> None:
        self._workspace_root = workspace_root
        self._on_edit = on_edit
        self._max_concurrent = max_concurrent
        self._edit_history = edit_history or EditHistoryLedger()

        self._lock = threading.Lock()
        self._file_locks: dict[str, threading.Lock] = {}
        self._metrics = ArbiterMetrics()
        self._generation = 0

    def record_conflict(self, reason: str = "") -> None:
        """Record one semantic write conflict for telemetry/observability."""
        with self._lock:
            self._metrics.conflicts_detected += 1

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

    def record_edit(
        self,
        file_path: str,
        actor_label: str = "",
        *,
        run_id: str = "",
        agent_run_id: str = "",
        task_id: str = "",
        agent_id: str | None = None,
        edit_type: str = "edit",
        old_hash: str = "",
        new_hash: str = "",
        description: str = "",
    ) -> int:
        """Record a successful edit. Returns the new generation.

        Writes directly to the internal edit-history ledger.
        """
        with self._lock:
            self._generation += 1
            gen = self._generation
            self._metrics.total_edits += 1

        try:
            self._edit_history.record(
                run_id=run_id,
                file_path=file_path,
                agent_run_id=agent_run_id,
                task_id=task_id,
                edit_type=edit_type,
                old_hash=old_hash,
                new_hash=new_hash,
                description=description,
            )
        except Exception:
            logger.debug("EditHistoryLedger.record failed for %s", file_path)

        if self._on_edit:
            try:
                actor = str(task_id or agent_run_id or agent_id or actor_label or "")
                self._on_edit(file_path, actor, gen)
            except Exception:
                logger.debug("on_edit callback failed for %s", file_path)

        return gen

    # -- Queries --------------------------------------------------------------

    @property
    def metrics(self) -> ArbiterMetrics:
        with self._lock:
            return ArbiterMetrics(
                total_edits=self._metrics.total_edits,
                conflicts_detected=self._metrics.conflicts_detected,
                active_locks=len(self._file_locks),
            )

    @property
    def active_lock_count(self) -> int:
        with self._lock:
            return sum(1 for lock in self._file_locks.values() if lock.locked())

    @property
    def generation(self) -> int:
        with self._lock:
            return self._generation

    @property
    def initialized(self) -> bool:
        return bool(getattr(self._edit_history, "initialized", False))

    def changes_in_scope(
        self,
        run_id: str,
        scope_prefixes: list[str],
        since: float,
    ) -> list[Any]:
        return self._edit_history.changes_in_scope(run_id, scope_prefixes, since)

    def external_changes_in_scope(
        self,
        run_id: str,
        scope_prefixes: list[str],
        since: float,
        exclude_run_id: str | None = None,
    ) -> list[Any]:
        return self._edit_history.external_changes_in_scope(
            run_id,
            scope_prefixes,
            since,
            exclude_run_id=exclude_run_id,
        )

    def changes_since(
        self,
        since: float,
        run_id: str | None = None,
    ) -> list[Any]:
        return self._edit_history.changes_since(since, run_id=run_id)

    def recent_edits(
        self,
        seconds: float = 60.0,
        run_id: str | None = None,
    ) -> list[Any]:
        return self._edit_history.recent_edits(seconds=seconds, run_id=run_id)

    def hotspots(
        self,
        limit: int = 10,
        run_id: str | None = None,
    ) -> list[tuple[str, int]]:
        return self._edit_history.hotspots(limit=limit, run_id=run_id)

    def who_changed(
        self,
        file_path: str,
        run_id: str | None = None,
    ) -> list[Any]:
        return self._edit_history.who_changed(file_path, run_id=run_id)

    def changes_by_agent_run(
        self,
        run_id: str,
        agent_run_id: str,
    ) -> list[Any]:
        return self._edit_history.changes_by_agent_run(run_id, agent_run_id)

    def contention_hotspots(
        self,
        scope_prefixes: list[str] | None = None,
        limit: int = 10,
        days: int = 7,
        run_id: str | None = None,
    ) -> list[Any]:
        return self._edit_history.contention_hotspots(
            scope_prefixes,
            limit=limit,
            days=days,
            run_id=run_id,
        )

    def status(self) -> dict[str, Any]:
        """Return arbiter status summary."""
        m = self.metrics
        return {
            "total_edits": m.total_edits,
            "conflicts_detected": m.conflicts_detected,
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
