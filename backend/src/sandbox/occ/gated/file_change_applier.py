"""Per-file serial applier for OCC-gated changes.

One :class:`FileChangeApplier` exists per workspace path. It serializes all
changes to that path under a ``threading.Lock`` and re-reads the file under the
lock at apply time so the gate sees concurrent writes that landed between
changeset construction and commit.

See ``.omc/plans/occ-changeset-gate-simplification.md`` §Gate algorithm.
"""

from __future__ import annotations

import asyncio
import threading
from collections.abc import Sequence

from sandbox.occ.changeset.types import (
    DeleteChange,
    EditChange,
    FileResult,
    FileStatus,
    GatedChange,
    WriteChange,
)
from sandbox.occ.content.hashing import content_hash
from sandbox.occ.content.manager import ContentManager


class FileChangeApplier:
    """Serial change applier for one workspace path."""

    def __init__(self, path: str, content: ContentManager) -> None:
        self._path = path
        self._content = content
        self._lock = threading.Lock()

    @property
    def path(self) -> str:
        return self._path

    async def apply_many(
        self,
        changes: Sequence[GatedChange],
    ) -> list[FileResult]:
        """Apply *changes* in submission order against the live file."""
        return await asyncio.to_thread(self._apply_many_sync, changes)

    # -- Internals ---------------------------------------------------------

    def _apply_many_sync(
        self,
        changes: Sequence[GatedChange],
    ) -> list[FileResult]:
        results: list[FileResult] = []
        with self._lock:
            for change in changes:
                current, existed = self._content.read(self._path, allow_missing=True)
                if isinstance(change, WriteChange):
                    results.append(self._apply_write(change, current, existed))
                elif isinstance(change, EditChange):
                    results.append(self._apply_edit(change, current, existed))
                elif isinstance(change, DeleteChange):
                    results.append(self._apply_delete(change, current, existed))
                else:  # pragma: no cover - exhaustive guard for the GatedChange union
                    results.append(
                        FileResult(
                            path=self._path,
                            status=FileStatus.FAILED,
                            message=f"unsupported change kind: {type(change).__name__}",
                        )
                    )
        return results

    def _apply_write(
        self,
        change: WriteChange,
        current: str,
        existed: bool,
    ) -> FileResult:
        # Three contracts encoded in (base_existed, base_hash):
        #   base_existed=False           -> create-only (abort if exists)
        #   base_existed=True, hash!=""  -> pinned modify (CAS by hash)
        #   base_existed=True, hash==""  -> blind overwrite/create under lock
        #                                   (API write path; no host-side base read)
        if not change.base_existed:
            if existed:
                return FileResult(
                    path=self._path,
                    status=FileStatus.ABORTED_VERSION,
                    message="existence changed",
                )
        elif change.base_hash:
            if not existed:
                return FileResult(
                    path=self._path,
                    status=FileStatus.ABORTED_VERSION,
                    message="existence changed",
                )
            if content_hash(current) != change.base_hash:
                return FileResult(
                    path=self._path,
                    status=FileStatus.ABORTED_VERSION,
                    message="content changed",
                )
        try:
            self._content.write(self._path, change.final_content)
        except Exception as exc:
            return FileResult(
                path=self._path,
                status=FileStatus.FAILED,
                message=str(exc),
            )
        return FileResult(path=self._path, status=FileStatus.COMMITTED)

    def _apply_edit(
        self,
        change: EditChange,
        current: str,
        existed: bool,
    ) -> FileResult:
        if not existed:
            return FileResult(
                path=self._path,
                status=FileStatus.ABORTED_OVERLAP,
                message="file does not exist",
            )
        result = current
        for edit in change.edits:
            count = result.count(edit.old_text)
            if count == 0:
                return FileResult(
                    path=self._path,
                    status=FileStatus.ABORTED_OVERLAP,
                    message="anchor not found",
                )
            if count >= 2:
                return FileResult(
                    path=self._path,
                    status=FileStatus.ABORTED_OVERLAP,
                    message="anchor ambiguous (multiple matches)",
                )
            result = result.replace(edit.old_text, edit.new_text, 1)
        try:
            self._content.write(self._path, result)
        except Exception as exc:
            return FileResult(
                path=self._path,
                status=FileStatus.FAILED,
                message=str(exc),
            )
        return FileResult(path=self._path, status=FileStatus.COMMITTED)

    def _apply_delete(
        self,
        change: DeleteChange,
        current: str,
        existed: bool,
    ) -> FileResult:
        if not existed:
            return FileResult(path=self._path, status=FileStatus.COMMITTED)
        if content_hash(current) != change.base_hash:
            return FileResult(
                path=self._path,
                status=FileStatus.ABORTED_VERSION,
                message="content changed before delete",
            )
        try:
            self._content.delete(self._path)
        except Exception as exc:
            return FileResult(
                path=self._path,
                status=FileStatus.FAILED,
                message=str(exc),
            )
        return FileResult(path=self._path, status=FileStatus.COMMITTED)


__all__ = ["FileChangeApplier"]
