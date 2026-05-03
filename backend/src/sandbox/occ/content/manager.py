"""Local/sandbox-aware file content reader and writer."""

from __future__ import annotations

from dataclasses import dataclass
import logging
import shutil
from pathlib import Path
from types import SimpleNamespace
from typing import Any

from sandbox.occ.content.hashing import content_hash
from sandbox.occ.content.path_utils import resolve_workspace_path

from sandbox.client.async_bridge import run_sync

logger = logging.getLogger(__name__)

FileReadResult = tuple[str, bool]
FileReadResults = dict[str, FileReadResult]

@dataclass(frozen=True)
class CheckedApplyChange:
    """One exact-base checked write/delete for a batch apply."""

    file_path: str
    base_hash: str
    base_existed: bool
    final_content: str | None


@dataclass(frozen=True)
class CheckedApplyResult:
    """Outcome of an exact-base checked batch apply."""

    success: bool
    conflict_path: str | None = None
    conflict_reason: str = ""
    message: str = ""


class ContentManager:
    """Read and write file content, routing to a sandbox when one is bound."""

    def __init__(
        self,
        workspace_root: str,
        sandbox: Any = None,
    ) -> None:
        self._workspace_root = str(workspace_root or "")
        self._sandbox = sandbox

    def bind_sandbox(self, sandbox: Any) -> None:
        """Update the sandbox handle for subsequent reads/writes."""
        self._sandbox = sandbox

    @property
    def workspace_root(self) -> str:
        return self._workspace_root

    def read(self, file_path: str, *, allow_missing: bool = False) -> FileReadResult:
        """Read *file_path* returning ``(content, existed)``."""
        resolved_path = self._resolve_path(file_path)
        fs = getattr(self._sandbox, "fs", None) if self._sandbox is not None else None
        if fs is not None and callable(getattr(fs, "download_file", None)):
            return self._read_fs(resolved_path, allow_missing=allow_missing)
        return self._read_local(resolved_path, allow_missing=allow_missing)

    def read_many(
        self,
        file_paths: list[str],
        *,
        allow_missing: bool = False,
    ) -> FileReadResults:
        """Read multiple files, batching transport or filesystem reads when possible."""
        unique_paths = list(dict.fromkeys(file_paths))
        if not unique_paths:
            return {}
        resolved_by_path = {path: self._resolve_path(path) for path in unique_paths}
        if self._sandbox is None:
            return {
                path: self._read_local(resolved_by_path[path], allow_missing=allow_missing)
                for path in unique_paths
            }
        resolved_paths = list(dict.fromkeys(resolved_by_path.values()))
        via_fs = self._read_fs_batch(resolved_paths, allow_missing=allow_missing)
        if via_fs is not None:
            return {path: via_fs[resolved_by_path[path]] for path in unique_paths}
        return {path: self.read(path, allow_missing=allow_missing) for path in unique_paths}

    def write(self, file_path: str, content: str) -> None:
        """Write *content* to *file_path*, preferring the sandbox when bound."""
        resolved_path = self._resolve_path(file_path)
        fs = getattr(self._sandbox, "fs", None) if self._sandbox is not None else None
        if fs is not None and callable(getattr(fs, "upload_file", None)):
            run_sync(fs.upload_file(content.encode("utf-8"), resolved_path))
            return
        path = Path(resolved_path)
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content, encoding="utf-8")

    def write_bytes(self, file_path: str, content: bytes) -> None:
        """Write raw bytes to *file_path*, preferring the sandbox when bound."""
        resolved_path = self._resolve_path(file_path)
        fs = getattr(self._sandbox, "fs", None) if self._sandbox is not None else None
        if fs is not None and callable(getattr(fs, "upload_file", None)):
            run_sync(fs.upload_file(content, resolved_path))
            return
        path = Path(resolved_path)
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(content)

    def delete(self, file_path: str) -> None:
        """Delete *file_path*, preferring the sandbox when one is bound."""
        resolved_path = self._resolve_path(file_path)
        fs = getattr(self._sandbox, "fs", None) if self._sandbox is not None else None
        delete_fn = getattr(fs, "delete_file", None) if fs is not None else None
        if callable(delete_fn):
            run_sync(delete_fn(resolved_path))
            return
        path = Path(resolved_path)
        try:
            path.unlink()
        except FileNotFoundError:
            return

    def delete_path(self, file_path: str) -> None:
        """Delete a file, symlink, or directory tree."""
        resolved_path = self._resolve_path(file_path)
        path = Path(resolved_path)
        try:
            if path.is_symlink() or path.is_file():
                path.unlink()
            elif path.is_dir():
                shutil.rmtree(path)
        except FileNotFoundError:
            return

    def make_symlink(self, file_path: str, target: str) -> None:
        """Replace *file_path* with a symlink to *target*."""
        resolved_path = self._resolve_path(file_path)
        path = Path(resolved_path)
        path.parent.mkdir(parents=True, exist_ok=True)
        self.delete_path(file_path)
        path.symlink_to(target)

    def list_child_names(self, file_path: str) -> list[str]:
        """Return direct child names for a directory, or an empty list."""
        resolved_path = self._resolve_path(file_path)
        path = Path(resolved_path)
        if not path.is_dir():
            return []
        return sorted(child.name for child in path.iterdir())

    def apply_many(self, changes: list[tuple[str, str | None]]) -> None:
        """Apply many writes/deletes through one sandbox round trip when possible."""
        if not changes:
            return
        for file_path, content in changes:
            if content is None:
                self.delete(file_path)
            else:
                self.write(file_path, content)

    def apply_many_with_base_check(
        self,
        changes: list[CheckedApplyChange],
    ) -> CheckedApplyResult:
        """Verify exact base hashes and apply all changes in one round trip.

        This is an optimization for clean OCC batches. It intentionally does
        not attempt merge fallback; callers should fall back to a full read path
        when it returns ``conflict_reason == "base_mismatch"``.
        """
        if not changes:
            return CheckedApplyResult(success=True)
        return self._apply_local_batch_checked(changes)

    # -- Private --------------------------------------------------------------

    def _resolve_path(self, file_path: str) -> str:
        return resolve_workspace_path(file_path, self._workspace_root)

    @staticmethod
    def _read_local(file_path: str, *, allow_missing: bool) -> FileReadResult:
        path = Path(file_path)
        if not path.exists():
            if allow_missing:
                return "", False
            raise FileNotFoundError(file_path)
        return path.read_text(encoding="utf-8"), True

    def _read_fs(self, file_path: str, *, allow_missing: bool) -> FileReadResult:
        fs = self._sandbox.fs
        try:
            payload = run_sync(fs.download_file(file_path))
        except FileNotFoundError:
            if allow_missing:
                return "", False
            raise
        if isinstance(payload, bytes):
            return payload.decode("utf-8"), True
        return str(payload), True

    def _read_fs_batch(
        self,
        file_paths: list[str],
        *,
        allow_missing: bool,
    ) -> FileReadResults | None:
        fs = getattr(self._sandbox, "fs", None)
        download_files_fn = getattr(fs, "download_files", None) if fs is not None else None
        if not callable(download_files_fn):
            return None

        try:
            requests = [SimpleNamespace(source=path) for path in file_paths]
            responses = run_sync(download_files_fn(requests))
        except Exception:
            logger.debug("Batch download_files failed", exc_info=True)
            return None

        payload_by_path: dict[str, Any] = {}
        for response in responses or ():
            source = getattr(response, "source", None)
            if isinstance(source, str):
                payload_by_path[source] = response

        results: FileReadResults = {}
        for path in file_paths:
            response = payload_by_path.get(path)
            if response is None or getattr(response, "error", None):
                if allow_missing:
                    results[path] = ("", False)
                    continue
                raise FileNotFoundError(path)
            payload = getattr(response, "result", None)
            if payload is None:
                if allow_missing:
                    results[path] = ("", False)
                    continue
                raise FileNotFoundError(path)
            content = payload.decode("utf-8") if isinstance(payload, bytes) else str(payload)
            results[path] = (content, True)
        return results

    def _apply_local_batch_checked(
        self,
        changes: list[CheckedApplyChange],
    ) -> CheckedApplyResult:
        backups: list[tuple[Path, bool, str]] = []
        for change in changes:
            if change.final_content is None and not change.base_existed:
                return CheckedApplyResult(
                    success=False,
                    conflict_path=change.file_path,
                    conflict_reason="base_mismatch",
                    message="file content changed before delete",
                )
            path = Path(self._resolve_path(change.file_path))
            try:
                current = path.read_text(encoding="utf-8")
            except FileNotFoundError:
                existed = False
                current = ""
                current_hash = ""
            else:
                existed = True
                current_hash = content_hash(current)
            backups.append((path, existed, current))
            if change.base_existed:
                if not existed or current_hash != change.base_hash:
                    return CheckedApplyResult(
                        success=False,
                        conflict_path=change.file_path,
                        conflict_reason="base_mismatch",
                        message="file content changed before checked apply",
                    )
            elif existed:
                return CheckedApplyResult(
                    success=False,
                    conflict_path=change.file_path,
                    conflict_reason="base_mismatch",
                    message="file already exists; base said it did not",
                )

        try:
            for change in changes:
                path = Path(self._resolve_path(change.file_path))
                if change.final_content is None:
                    try:
                        path.unlink()
                    except FileNotFoundError:
                        pass
                    continue
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(change.final_content, encoding="utf-8")
        except Exception as exc:
            for path, existed, content in reversed(backups):
                try:
                    if existed:
                        path.parent.mkdir(parents=True, exist_ok=True)
                        path.write_text(content, encoding="utf-8")
                    else:
                        path.unlink(missing_ok=True)
                except Exception:
                    pass
            return CheckedApplyResult(
                success=False,
                conflict_reason="write_failed",
                message=str(exc),
            )
        return CheckedApplyResult(success=True)
