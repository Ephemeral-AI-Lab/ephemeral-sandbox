"""Semantic write pipeline: resolve, commit, refresh, abort.

The coordinator owns service-level semantic writes for a single
:class:`CodeIntelligenceService` sandbox. Daytona write tools no longer call
this module directly; they execute one process command through the unified
process-audit entry point.
"""

from __future__ import annotations

import logging
import time
from collections.abc import Iterable, Sequence
from typing import Any

from code_intelligence.core.hashing import content_hash
from code_intelligence.mutations.arbiter import Arbiter
from code_intelligence.mutations.time_machine import TimeMachine
from code_intelligence.mutations.content_manager import (
    CheckedApplyChange,
    ContentManager,
)
from code_intelligence.mutations.write_coordinator.models import (
    CommitOperation,
    ResolvedChange,
)
from code_intelligence.mutations.write_coordinator.resolver import ChangeResolver
from code_intelligence.mutations.write_coordinator.results import (
    edit_result,
    operation_abort,
)
from code_intelligence.core.types import (
    EditResult,
    OperationChange,
    OperationResult,
)

logger = logging.getLogger(__name__)


class WriteCoordinator:
    """Encapsulates the semantic write pipeline for one sandbox."""

    def __init__(
        self,
        *,
        arbiter: Arbiter,
        time_machine: TimeMachine,
        symbol_index: Any,
        lsp_client: Any,
        content: ContentManager,
    ) -> None:
        self._arbiter = arbiter
        self._time_machine = time_machine
        self._symbol_index = symbol_index
        self._lsp_client = lsp_client
        self._content = content
        self._resolver = ChangeResolver()

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

        Thin wrapper over :meth:`commit_many_operations_against_base`.

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
            Any unmergeable mismatch aborts the *whole* operation, so partial
            multi-file edits are never left on disk. Setting ``strict_base=True`` on a
            change skips the merge fallback entirely and aborts on any hash
            mismatch (used for whole-file rewrites like ``move --overwrite``).
        """
        if not changes:
            return OperationResult(
                success=True,
                status="committed",
                files=(),
                conflict_file=None,
                conflict_reason="",
                timings={"total": 0.0},
            )
        [result] = self.commit_many_operations_against_base(
            [
                CommitOperation(
                    changes=tuple(changes),
                    agent_id=agent_id,
                    edit_type=edit_type,
                    description=description,
                )
            ]
        )
        return result


    def commit_many_operations_against_base(
        self,
        operations: Sequence[CommitOperation],
    ) -> list[OperationResult]:
        """Commit multiple disjoint operations with batched sandbox I/O."""
        ops = list(operations)
        if not ops:
            return []

        path_to_owner: dict[str, int] = {}
        overlap = False
        for idx, op in enumerate(ops):
            for change in op.changes:
                owner = path_to_owner.setdefault(change.file_path, idx)
                if owner != idx:
                    overlap = True
                    break
            if overlap:
                break
        if overlap:
            results: list[OperationResult] = []
            for op in ops:
                [r] = self.commit_many_operations_against_base([op])
                results.append(r)
            return results

        all_paths = sorted(path_to_owner)
        started = time.perf_counter()
        timings: dict[str, float] = {}
        lock_started = time.perf_counter()
        held, lock_conflict = self._acquire_locks(all_paths)
        timings["lock_wait"] = round(time.perf_counter() - lock_started, 6)
        if lock_conflict is not None:
            timings["total"] = round(time.perf_counter() - started, 6)
            return [
                operation_abort(
                    op.changes,
                    status="aborted_lock",
                    conflict_file=(
                        lock_conflict
                        if any(c.file_path == lock_conflict for c in op.changes)
                        else None
                    ),
                    conflict_reason="could not acquire file lock (timeout)",
                    timings=timings,
                )
                for op in ops
            ]

        try:
            fast_results = self._try_commit_many_exact_base_fast(
                ops,
                timings=timings,
                started=started,
            )
            if fast_results is not None:
                return fast_results

            read_started = time.perf_counter()
            current_by_path = self._content.read_many(all_paths, allow_missing=True)
            timings["resolve_read"] = round(time.perf_counter() - read_started, 6)

            resolved_by_op: list[list[ResolvedChange] | None] = []
            results: list[OperationResult | None] = [None] * len(ops)
            resolve_started = time.perf_counter()
            for idx, op in enumerate(ops):
                resolved: list[ResolvedChange] = []
                aborted = False
                for change in op.changes:
                    current_now, existed_now = current_by_path.get(
                        change.file_path,
                        ("", False),
                    )
                    resolved_change, conflict = self._resolver.resolve_change(
                        change,
                        current_now,
                        existed_now,
                    )
                    if conflict is not None:
                        status, reason = conflict
                        self._arbiter.record_conflict(status)
                        results[idx] = operation_abort(
                            op.changes,
                            status=status,
                            conflict_file=change.file_path,
                            conflict_reason=reason,
                            timings=timings,
                        )
                        aborted = True
                        break
                    assert resolved_change is not None
                    resolved.append(resolved_change)
                resolved_by_op.append(None if aborted else resolved)
            timings["resolve"] = round(time.perf_counter() - resolve_started, 6)

            apply_items: list[tuple[str, str | None]] = []
            rollback_items: list[tuple[str, str | None]] = []
            for resolved_items in resolved_by_op:
                if resolved_items is None:
                    continue
                for item in resolved_items:
                    self._time_machine.save(
                        item.change.file_path,
                        item.current_content,
                        existed=item.existed,
                    )
                    apply_items.append((item.change.file_path, item.final_content))
                    rollback_items.append(
                        (item.change.file_path, item.current_content if item.existed else None),
                    )

            apply_started = time.perf_counter()
            try:
                self._content.apply_many(apply_items)
            except Exception as exc:
                try:
                    self._content.apply_many(list(reversed(rollback_items)))
                except Exception:  # pragma: no cover - best effort rollback
                    logger.exception("batch rollback failed")
                timings["apply"] = round(time.perf_counter() - apply_started, 6)
                timings["total"] = round(time.perf_counter() - started, 6)
                for idx, op in enumerate(ops):
                    if results[idx] is None:
                        results[idx] = OperationResult(
                            success=False,
                            status="failed",
                            files=tuple(
                                edit_result(c.file_path, f"batch operation failed: {exc}")
                                for c in op.changes
                            ),
                            conflict_file=None,
                            conflict_reason=f"write failed: {exc}",
                            timings=dict(timings),
                        )
                return [r for r in results if r is not None]
            timings["apply"] = round(time.perf_counter() - apply_started, 6)

            record_started = time.perf_counter()
            for idx, op in enumerate(ops):
                if results[idx] is not None:
                    continue
                resolved_items = resolved_by_op[idx]
                if resolved_items is None:
                    continue
                commit_results: list[EditResult] = []
                for item in resolved_items:
                    change = item.change
                    new_hash = (
                        content_hash(item.final_content) if item.final_content is not None else ""
                    )
                    gen = self._arbiter.record_edit(
                        file_path=change.file_path,
                        actor_label=op.agent_id,
                        edit_type=op.edit_type,
                        old_hash=item.current_hash if item.existed else "",
                        new_hash=new_hash,
                        description=op.description,
                    )
                    self._symbol_index.refresh(change.file_path, item.final_content)
                    self._lsp_client.invalidate(change.file_path)
                    commit_results.append(
                        edit_result(
                            change.file_path,
                            "Wrote file",
                            success=True,
                            snapshot_id=str(gen),
                        ),
                    )
                results[idx] = OperationResult(
                    success=True,
                    status="committed",
                    files=tuple(commit_results),
                    conflict_file=None,
                    conflict_reason="",
                    timings=dict(timings),
                )
            timings["apply_record"] = round(time.perf_counter() - record_started, 6)
            timings["total"] = round(time.perf_counter() - started, 6)
            for idx, result in enumerate(results):
                if result is not None:
                    result.timings.update(timings)
                elif not ops[idx].changes:
                    results[idx] = OperationResult(
                        success=True,
                        status="committed",
                        files=(),
                        conflict_file=None,
                        conflict_reason="",
                        timings=dict(timings),
                    )
            return [r for r in results if r is not None]
        finally:
            for fp in reversed(held):
                self._arbiter.release_file_lock(fp)

    def _try_commit_many_exact_base_fast(
        self,
        ops: Sequence[CommitOperation],
        *,
        timings: dict[str, float],
        started: float,
    ) -> list[OperationResult] | None:
        """Commit a clean disjoint batch via checked apply, or fall back.

        The normal path reads full current content after locks so it can merge
        non-overlapping drift. Most tool batches are clean: the plan-time base
        still matches current content. For those cases, a single sandbox call
        can verify hashes and apply all changes; on base mismatch we return
        ``None`` so the existing full read/merge path preserves behavior.
        """
        checked: list[CheckedApplyChange] = []
        for op in ops:
            for change in op.changes:
                if change.final_content is None and not change.base_existed:
                    return None
                checked.append(
                    CheckedApplyChange(
                        file_path=change.file_path,
                        base_hash=change.base_hash,
                        base_existed=change.base_existed,
                        final_content=change.final_content,
                    )
                )

        if not checked:
            return None

        apply_started = time.perf_counter()
        try:
            apply_result = self._content.apply_many_with_base_check(checked)
        except Exception as exc:  # pragma: no cover - defensive I/O
            timings["apply"] = round(time.perf_counter() - apply_started, 6)
            timings["total"] = round(time.perf_counter() - started, 6)
            return [
                OperationResult(
                    success=False,
                    status="failed",
                    files=tuple(
                        edit_result(c.file_path, f"checked batch operation failed: {exc}")
                        for c in op.changes
                    ),
                    conflict_file=None,
                    conflict_reason=f"write failed: {exc}",
                    timings=dict(timings),
                )
                for op in ops
            ]
        timings["apply"] = round(time.perf_counter() - apply_started, 6)

        if not apply_result.success:
            if apply_result.conflict_reason in {"base_mismatch", "unsupported"}:
                return None
            timings["total"] = round(time.perf_counter() - started, 6)
            return [
                OperationResult(
                    success=False,
                    status="failed",
                    files=tuple(
                        edit_result(
                            c.file_path,
                            apply_result.message or "checked batch operation failed",
                        )
                        for c in op.changes
                    ),
                    conflict_file=apply_result.conflict_path,
                    conflict_reason=apply_result.message or apply_result.conflict_reason,
                    timings=dict(timings),
                )
                for op in ops
            ]

        timings["resolve_read"] = 0.0
        timings["resolve"] = 0.0
        record_started = time.perf_counter()
        results: list[OperationResult] = []
        for op in ops:
            commit_results: list[EditResult] = []
            for change in op.changes:
                current_hash = change.base_hash if change.base_existed else ""
                new_hash = (
                    content_hash(change.final_content) if change.final_content is not None else ""
                )
                gen = self._arbiter.record_edit(
                    file_path=change.file_path,
                    actor_label=op.agent_id,
                    edit_type=op.edit_type,
                    old_hash=current_hash,
                    new_hash=new_hash,
                    description=op.description,
                )
                self._time_machine.save(
                    change.file_path,
                    change.base_content if change.base_existed else "",
                    existed=change.base_existed,
                )
                self._symbol_index.refresh(change.file_path, change.final_content)
                self._lsp_client.invalidate(change.file_path)
                commit_results.append(
                    edit_result(
                        change.file_path,
                        "Wrote file",
                        success=True,
                        snapshot_id=str(gen),
                    ),
                )
            results.append(
                OperationResult(
                    success=True,
                    status="committed",
                    files=tuple(commit_results),
                    conflict_file=None,
                    conflict_reason="",
                    timings=dict(timings),
                )
            )
        timings["apply_record"] = round(time.perf_counter() - record_started, 6)
        timings["total"] = round(time.perf_counter() - started, 6)
        for result in results:
            result.timings.update(timings)
        return results

    def _acquire_locks(self, file_paths: Iterable[str]) -> tuple[list[str], str | None]:
        """Acquire file locks in caller-provided order.

        Returns ``(held, None)`` on success or ``([], conflict_path)`` after
        releasing any prefix that was already acquired.
        """
        held: list[str] = []
        for file_path in file_paths:
            if not self._arbiter.acquire_file_lock(file_path):
                for prev in reversed(held):
                    self._arbiter.release_file_lock(prev)
                self._arbiter.record_conflict("lock_timeout")
                return [], file_path
            held.append(file_path)
        return held, None

    def undo_last_edit(self, file_path: str) -> EditResult:
        """Undo the last edit to *file_path* via TimeMachine."""
        snapshot = self._time_machine.rollback(file_path)
        if snapshot is None:
            return edit_result(file_path, "No snapshot available for undo")
        try:
            if snapshot.existed:
                self._content.write(file_path, snapshot.content)
            else:
                self._content.delete(file_path)
        except Exception as exc:
            return edit_result(file_path, f"Undo write failed: {exc}")
        self._symbol_index.refresh(file_path, snapshot.content if snapshot.existed else None)
        self._lsp_client.invalidate(file_path)
        return edit_result(file_path, "Reverted to previous snapshot", success=True)
