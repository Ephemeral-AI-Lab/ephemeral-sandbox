"""Legacy semantic write pipeline: resolve, commit, refresh, abort.

The coordinator owns service-level semantic writes for a single
:class:`CodeIntelligenceService` sandbox. Daytona write tools no longer call
this module directly; they execute one process command through the unified
process-audit entry point.
"""

from __future__ import annotations

import logging
import time
from collections.abc import Sequence
from typing import Any

from code_intelligence.hashing import content_hash
from code_intelligence.editing.arbiter import Arbiter
from code_intelligence.editing.merge import (
    detect_edit_window,
    merge_non_overlapping_edit,
)
from code_intelligence.editing.patcher import Patcher
from code_intelligence.editing.time_machine import TimeMachine
from code_intelligence.routing.content_manager import ContentManager
from code_intelligence.types import (
    EditResult,
    OperationChange,
    OperationResult,
)

logger = logging.getLogger(__name__)


def _result(
    file_path: str,
    message: str,
    *,
    success: bool = False,
    conflict: bool = False,
    conflict_reason: str = "",
    snapshot_id: str = "",
    timings: dict[str, float] | None = None,
) -> EditResult:
    return EditResult(
        success=success,
        file_path=file_path,
        message=message,
        conflict=conflict,
        conflict_reason=conflict_reason,
        snapshot_id=snapshot_id,
        timings=dict(timings or {}),
    )


def _record_timing(timings: dict[str, float], key: str, started_at: float) -> None:
    timings[key] = round(time.perf_counter() - started_at, 6)


def _conflict_result(
    arbiter: Arbiter,
    file_path: str,
    message: str,
    *,
    conflict_reason: str,
    snapshot_id: str = "",
    timings: dict[str, float] | None = None,
) -> EditResult:
    arbiter.record_conflict(conflict_reason)
    return _result(
        file_path,
        message,
        conflict=True,
        conflict_reason=conflict_reason,
        snapshot_id=snapshot_id,
        timings=timings,
    )


class WriteCoordinator:
    """Encapsulates the legacy semantic write pipeline for one sandbox."""

    def __init__(
        self,
        *,
        arbiter: Arbiter,
        time_machine: TimeMachine,
        patcher: Patcher,
        symbol_index: Any,
        lsp_client: Any,
        content: ContentManager,
    ) -> None:
        self._arbiter = arbiter
        self._time_machine = time_machine
        self._patcher = patcher
        self._symbol_index = symbol_index
        self._lsp_client = lsp_client
        self._content = content

    # -- Semantic operation primitives ---------------------------------------

    def commit_operation_against_base(
        self,
        changes: Sequence[OperationChange],
        *,
        agent_id: str = "",
        edit_type: str,
        description: str = "",
    ) -> OperationResult:
        """Atomically commit one tool operation against per-file bases.

        Semantics:
          * **Sorted-path locking** — acquire all per-file locks in sorted
            path order; release in reverse on exit.
          * **Delete branch** — ``final_content is None`` means delete. Requires
            ``current_hash == base_hash`` exactly; any mismatch aborts.
          * **Create branch** — ``base_existed=False`` with ``base_content==""``.
            Aborts if the file already exists on disk.
          * **Modify branch** — if a file's current hash equals its
            ``base_hash`` the operation takes ``final_content`` verbatim; otherwise
            it tries a non-overlapping merge (same policy as a single-file merge).
            Any unmergeable mismatch aborts the *whole* operation — no partial
            rename is ever left on disk. Setting ``strict_base=True`` on a
            change skips the merge fallback entirely and aborts on any hash
            mismatch (used for whole-file rewrites like ``move --overwrite``).
          * **Two-pass commit** — resolved contents are staged in memory
            first, then written + recorded + refreshed. A write failure
            during the commit pass triggers best-effort TimeMachine
            rollback of earlier files and returns ``status="failed"``.
        """
        started = time.perf_counter()
        timings: dict[str, float] = {}

        if not changes:
            return OperationResult(
                success=True,
                status="committed",
                files=(),
                conflict_file=None,
                conflict_reason="",
                timings={"total": 0.0},
            )

        sorted_changes = sorted(changes, key=lambda c: c.file_path)

        # 2. Acquire locks in sorted order; release prefix on timeout.
        held: list[str] = []
        lock_started = time.perf_counter()
        for change in sorted_changes:
            if not self._arbiter.acquire_file_lock(change.file_path):
                for prev in reversed(held):
                    self._arbiter.release_file_lock(prev)
                self._arbiter.record_conflict("lock_timeout")
                timings["lock_wait"] = round(time.perf_counter() - lock_started, 6)
                timings["total"] = round(time.perf_counter() - started, 6)
                return self._operation_abort(
                    changes,
                    status="aborted_lock",
                    conflict_file=change.file_path,
                    conflict_reason="could not acquire file lock (timeout)",
                    timings=timings,
                )
            held.append(change.file_path)
        timings["lock_wait"] = round(time.perf_counter() - lock_started, 6)

        try:
            # 3. Resolve every file against its plan-time base, staging in
            #    memory. Any unmergeable file aborts the operation before we
            #    touch disk.
            resolve_started = time.perf_counter()
            # (change, current_now, resolved_content_or_None, current_hash, existed_now)
            resolved: list[tuple[OperationChange, str, str | None, str, bool]] = []
            for change in sorted_changes:
                try:
                    current_now, existed_now = self._content.read(
                        change.file_path, allow_missing=True,
                    )
                except Exception as exc:  # pragma: no cover - defensive I/O
                    timings["resolve"] = round(time.perf_counter() - resolve_started, 6)
                    timings["total"] = round(time.perf_counter() - started, 6)
                    return self._operation_abort(
                        changes,
                        status="failed",
                        conflict_file=change.file_path,
                        conflict_reason=f"read failed: {exc}",
                        timings=timings,
                    )

                current_hash = content_hash(current_now) if existed_now else ""

                # --- Delete branch ---
                if change.final_content is None:
                    if not existed_now or current_hash != change.base_hash:
                        self._arbiter.record_conflict("aborted_version")
                        timings["resolve"] = round(time.perf_counter() - resolve_started, 6)
                        timings["total"] = round(time.perf_counter() - started, 6)
                        return self._operation_abort(
                            changes,
                            status="aborted_version",
                            conflict_file=change.file_path,
                            conflict_reason="file content changed before delete",
                            timings=timings,
                        )
                    resolved.append((change, current_now, None, current_hash, existed_now))
                    continue

                # --- Create branch ---
                if not change.base_existed:
                    if existed_now:
                        self._arbiter.record_conflict("aborted_version")
                        timings["resolve"] = round(time.perf_counter() - resolve_started, 6)
                        timings["total"] = round(time.perf_counter() - started, 6)
                        return self._operation_abort(
                            changes,
                            status="aborted_version",
                            conflict_file=change.file_path,
                            conflict_reason="file already exists; base said it did not",
                            timings=timings,
                        )
                    resolved.append((change, current_now, change.final_content, "", False))
                    continue

                # --- Modify branch ---
                if existed_now and current_hash == change.base_hash:
                    resolved_content: str = change.final_content
                elif change.strict_base:
                    self._arbiter.record_conflict("aborted_version")
                    timings["resolve"] = round(time.perf_counter() - resolve_started, 6)
                    timings["total"] = round(time.perf_counter() - started, 6)
                    return self._operation_abort(
                        changes,
                        status="aborted_version",
                        conflict_file=change.file_path,
                        conflict_reason=(
                            "file content changed since base was captured "
                            "(strict_base=True)"
                        ),
                        timings=timings,
                    )
                else:
                    resolved_content, conflict = self._resolve_semantic_change(
                        change, current_now, existed_now,
                    )
                    if conflict is not None:
                        status, reason = conflict
                        self._arbiter.record_conflict(status)
                        timings["resolve"] = round(time.perf_counter() - resolve_started, 6)
                        timings["total"] = round(time.perf_counter() - started, 6)
                        return self._operation_abort(
                            changes,
                            status=status,
                            conflict_file=change.file_path,
                            conflict_reason=reason,
                            timings=timings,
                        )
                resolved.append(
                    (change, current_now, resolved_content, current_hash, existed_now),
                )
            timings["resolve"] = round(time.perf_counter() - resolve_started, 6)

            # 4. Commit pass. A mid-operation I/O failure triggers best-effort
            #    rollback of already-written files via TimeMachine.
            apply_started = time.perf_counter()
            commit_results: list[EditResult] = []
            committed_paths: list[str] = []
            for change, current_now, resolved_content, current_hash, existed_now in resolved:
                per_timings: dict[str, float] = {}
                per_started = time.perf_counter()
                self._time_machine.save(change.file_path, current_now)
                try:
                    if resolved_content is None:
                        self._content.delete(change.file_path)
                    else:
                        self._content.write(change.file_path, resolved_content)
                except Exception as exc:
                    for fp in reversed(committed_paths):
                        snap = self._time_machine.rollback(fp)
                        if snap is None:
                            continue
                        try:
                            self._content.write(fp, snap.content)
                        except Exception:  # pragma: no cover - best effort
                            logger.exception("rollback failed for %s", fp)
                    timings["apply"] = round(time.perf_counter() - apply_started, 6)
                    timings["total"] = round(time.perf_counter() - started, 6)
                    return OperationResult(
                        success=False,
                        status="failed",
                        files=tuple(
                            _result(c.file_path, f"operation failed on {change.file_path}: {exc}")
                            for c in sorted_changes
                        ),
                        conflict_file=change.file_path,
                        conflict_reason=f"write failed: {exc}",
                        timings=timings,
                    )
                committed_paths.append(change.file_path)
                new_hash = content_hash(resolved_content) if resolved_content is not None else ""
                gen = self._arbiter.record_edit(
                    file_path=change.file_path,
                    actor_label=agent_id,
                    edit_type=edit_type,
                    old_hash=current_hash if existed_now else "",
                    new_hash=new_hash,
                    description=description,
                )
                self._symbol_index.refresh(change.file_path, resolved_content or "")
                self._lsp_client.invalidate(change.file_path)
                per_timings["total"] = round(time.perf_counter() - per_started, 6)
                commit_results.append(
                    _result(
                        change.file_path,
                        "Wrote file",
                        success=True,
                        snapshot_id=str(gen),
                        timings=per_timings,
                    ),
                )
            timings["apply"] = round(time.perf_counter() - apply_started, 6)
            timings["total"] = round(time.perf_counter() - started, 6)
            return OperationResult(
                success=True,
                status="committed",
                files=tuple(commit_results),
                conflict_file=None,
                conflict_reason="",
                timings=timings,
            )
        finally:
            for fp in reversed(held):
                self._arbiter.release_file_lock(fp)

    @staticmethod
    def _merge_against_base(
        base_content: str | None,
        final_content: str,
        current_content: str | None,
    ) -> tuple[str | None, str]:
        """Attempt a non-overlapping merge of *final_content* onto *current_content*.

        Returns ``(merged, reason_kind)`` where *reason_kind* is one of:

        * ``""``            — success; *merged* is the resulting content.
        * ``"missing"``     — base or current is ``None``; cannot merge.
        * ``"unwindowable"``— ``detect_edit_window`` returned no window.
        * ``"overlap"``     — ``merge_non_overlapping_edit`` returned ``None``.
        """
        if base_content is None or current_content is None:
            return None, "missing"
        line_start, line_end, op = detect_edit_window(base_content, final_content)
        if line_start is None:
            return None, "unwindowable"
        merged = merge_non_overlapping_edit(
            original_content=base_content,
            new_content=final_content,
            current_content=current_content,
            line_start=line_start,
            line_end=line_end,
            operation_type=op,
        )
        if merged is None:
            return None, "overlap"
        return merged, ""

    def _resolve_semantic_change(
        self,
        change: OperationChange,
        current_now: str,
        existed_now: bool,
    ) -> tuple[str, tuple[str, str] | None]:
        """Resolve one file's final content against a possibly-changed base.

        Returns ``(resolved_content, None)`` on success or
        ``("", (status, reason))`` describing the abort class.
        """
        if not existed_now:
            return "", (
                "aborted_version",
                "file was deleted since rename plan was built",
            )
        assert change.final_content is not None  # modify branch only
        merged, reason_kind = self._merge_against_base(
            change.base_content, change.final_content, current_now,
        )
        if reason_kind == "":
            assert merged is not None
            return merged, None
        if reason_kind == "overlap":
            return "", (
                "aborted_overlap",
                "concurrent edit overlaps the rename window",
            )
        # "missing" or "unwindowable"
        return "", (
            "aborted_version",
            "base content changed and rewrite is whole-file / un-windowable",
        )

    @staticmethod
    def _operation_abort(
        changes: Sequence[OperationChange],
        *,
        status: str,
        conflict_file: str | None,
        conflict_reason: str,
        timings: dict[str, float],
    ) -> OperationResult:
        is_conflict = status.startswith("aborted")
        files = tuple(
            _result(
                c.file_path,
                conflict_reason,
                conflict=is_conflict,
                conflict_reason=status if is_conflict else "",
            )
            for c in changes
        )
        return OperationResult(
            success=False,
            status=status,  # type: ignore[arg-type]
            files=files,
            conflict_file=conflict_file,
            conflict_reason=conflict_reason,
            timings=dict(timings),
        )

    def undo_last_edit(self, file_path: str) -> EditResult:
        """Undo the last edit to *file_path* via TimeMachine."""
        snapshot = self._time_machine.rollback(file_path)
        if snapshot is None:
            return _result(file_path, "No snapshot available for undo")
        try:
            self._content.write(file_path, snapshot.content)
        except Exception as exc:
            return _result(file_path, f"Undo write failed: {exc}")
        self._symbol_index.refresh(file_path, snapshot.content)
        self._lsp_client.invalidate(file_path)
        return _result(file_path, "Reverted to previous snapshot", success=True)
