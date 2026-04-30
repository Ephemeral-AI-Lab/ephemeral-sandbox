"""Local/sandbox-aware file content reader and writer."""

from __future__ import annotations

import base64
from dataclasses import dataclass
import json
import logging
import shlex
import uuid
from pathlib import Path
from typing import Any

from sandbox.code_intelligence.core.hashing import content_hash
from sandbox.code_intelligence.core.path_utils import resolve_workspace_path
from sandbox.daytona_utils import (
    _REMOTE_WRITE_CHUNK_BYTES,
    _build_append_text_file_chunk_command,
    _build_read_text_file_command,
    _build_remove_file_command,
    _build_truncate_text_file_command,
    _build_write_text_file_commands,
    _extract_exit_code,
    _wrap_bash_command,
)

from sandbox.async_bridge import run_sync

logger = logging.getLogger(__name__)

FileReadResult = tuple[str, bool]
FileReadResults = dict[str, FileReadResult]


def _is_real_daytona_fs(fs: Any) -> bool:
    """Best-effort check that *fs* is Daytona SDK, not a local test double."""
    mod = getattr(type(fs), "__module__", "") or ""
    return "daytona" in mod


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


# Apply-script bodies. The same body runs whether ``ops`` was loaded from an
# inline base64 literal (small payloads) or a staged tmp file (large payloads
# that would overflow ARG_MAX/E2BIG if inlined into ``python3 -c`` argv).
_APPLY_BODY = """
for item in ops:
    path = pathlib.Path(item["path"])
    content_b64 = item.get("content_b64")
    if content_b64 is None:
        try:
            path.unlink()
        except FileNotFoundError:
            pass
        continue
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(base64.b64decode(content_b64))
"""

_CHECKED_APPLY_BODY = """
backups = []
for item in ops:
    path = pathlib.Path(item["path"])
    original_path = item.get("original_path") or item["path"]
    if item.get("content_b64") is None and not item.get("base_existed"):
        print(json.dumps({
            "ok": False,
            "reason": "base_mismatch",
            "path": original_path,
            "message": "file content changed before delete",
        }))
        raise SystemExit(0)
    try:
        current = path.read_text(encoding="utf-8")
    except FileNotFoundError:
        existed = False
        current = ""
        current_hash = ""
    else:
        existed = True
        current_hash = hashlib.sha256(current.encode("utf-8")).hexdigest()[:16]
    backups.append({
        "path": item["path"],
        "existed": existed,
        "content_b64": base64.b64encode(current.encode("utf-8")).decode("ascii"),
    })
    if item.get("base_existed"):
        if (not existed) or current_hash != item.get("base_hash", ""):
            print(json.dumps({
                "ok": False,
                "reason": "base_mismatch",
                "path": original_path,
                "message": "file content changed before checked apply",
            }))
            raise SystemExit(0)
    elif existed:
        print(json.dumps({
            "ok": False,
            "reason": "base_mismatch",
            "path": original_path,
            "message": "file already exists; base said it did not",
        }))
        raise SystemExit(0)

try:
    for item in ops:
        path = pathlib.Path(item["path"])
        content_b64 = item.get("content_b64")
        if content_b64 is None:
            try:
                path.unlink()
            except FileNotFoundError:
                pass
            continue
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(base64.b64decode(content_b64))
except Exception as exc:
    for backup in reversed(backups):
        path = pathlib.Path(backup["path"])
        try:
            if backup.get("existed"):
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(base64.b64decode(backup["content_b64"]))
            else:
                try:
                    path.unlink()
                except FileNotFoundError:
                    pass
        except Exception:
            pass
    print(json.dumps({
        "ok": False,
        "reason": "write_failed",
        "path": "",
        "message": str(exc),
    }))
    raise SystemExit(0)

print(json.dumps({"ok": True}))
"""

_FROM_FILE_PRELUDE = """
import base64
import hashlib
import json
import pathlib
import sys

ops = json.loads(pathlib.Path(sys.argv[1]).read_text(encoding="utf-8"))
"""

_BATCH_APPLY_FROM_FILE_SCRIPT = _FROM_FILE_PRELUDE + _APPLY_BODY
_CHECKED_BATCH_APPLY_FROM_FILE_SCRIPT = _FROM_FILE_PRELUDE + _CHECKED_APPLY_BODY


def _build_inline_apply_script(payload_bytes: bytes, body: str) -> str:
    """Compose an inline-payload apply script (small batches; avoids tmp file)."""
    encoded = base64.b64encode(payload_bytes).decode("ascii")
    prelude = (
        "import base64\n"
        "import hashlib\n"
        "import json\n"
        "import pathlib\n"
        "\n"
        f"ops = json.loads(base64.b64decode({encoded!r}).decode(\"utf-8\"))\n"
    )
    return prelude + body


def _parse_checked_apply_response(cleaned: str, exit_code: int | None) -> CheckedApplyResult:
    """Decode the JSON envelope emitted by ``_CHECKED_APPLY_BODY``."""
    if exit_code not in (0, None):
        raise RuntimeError(cleaned or "checked batch apply failed")
    payload_out = json.loads(cleaned or "{}")
    if not isinstance(payload_out, dict):
        raise RuntimeError("checked batch apply returned invalid JSON")
    if payload_out.get("ok"):
        return CheckedApplyResult(success=True)
    return CheckedApplyResult(
        success=False,
        conflict_path=str(payload_out.get("path") or "") or None,
        conflict_reason=str(payload_out.get("reason") or "failed"),
        message=str(payload_out.get("message") or ""),
    )


class ContentManager:
    """Read and write file content, routing to a sandbox when one is bound."""

    def __init__(self, workspace_root: str, sandbox: Any = None) -> None:
        self._workspace_root = str(workspace_root or "")
        self._sandbox = sandbox

    def bind_sandbox(self, sandbox: Any) -> None:
        """Update the sandbox handle for subsequent reads/writes."""
        self._sandbox = sandbox

    def read(self, file_path: str, *, allow_missing: bool = False) -> FileReadResult:
        """Read *file_path* returning ``(content, existed)``."""
        resolved_path = self._resolve_path(file_path)
        if self._sandbox is None:
            return self._read_local(resolved_path, allow_missing=allow_missing)
        if getattr(self._sandbox, "process", None) is None:
            fs = getattr(self._sandbox, "fs", None)
            if fs is not None and callable(getattr(fs, "download_file", None)):
                return self._read_fs(resolved_path, allow_missing=allow_missing)
            return self._read_local(resolved_path, allow_missing=allow_missing)
        try:
            return self._read_remote(resolved_path, allow_missing=allow_missing)
        except json.JSONDecodeError:
            logger.debug("Process read returned non-JSON output", exc_info=True)
            fs = getattr(self._sandbox, "fs", None)
            if fs is None or not callable(getattr(fs, "download_file", None)):
                raise
            return self._read_fs(resolved_path, allow_missing=allow_missing)

    def read_many(
        self,
        file_paths: list[str],
        *,
        allow_missing: bool = False,
    ) -> FileReadResults:
        """Read multiple files, batching remote sandbox reads when possible."""
        unique_paths = list(dict.fromkeys(file_paths))
        if not unique_paths:
            return {}
        resolved_by_path = {path: self._resolve_path(path) for path in unique_paths}
        if self._sandbox is None:
            return {
                path: self._read_local(resolved_by_path[path], allow_missing=allow_missing)
                for path in unique_paths
            }
        if getattr(self._sandbox, "process", None) is None:
            return {path: self.read(path, allow_missing=allow_missing) for path in unique_paths}
        resolved_paths = list(dict.fromkeys(resolved_by_path.values()))
        via_fs = self._read_fs_batch(resolved_paths, allow_missing=allow_missing)
        if via_fs is not None:
            return {path: via_fs[resolved_by_path[path]] for path in unique_paths}
        try:
            remote = self._read_remote_batch(resolved_paths, allow_missing=allow_missing)
        except json.JSONDecodeError:
            logger.debug("Batch process read returned non-JSON output", exc_info=True)
            fs = getattr(self._sandbox, "fs", None)
            if fs is None or not callable(getattr(fs, "download_file", None)):
                raise
            return {
                path: self._read_fs(resolved_by_path[path], allow_missing=allow_missing)
                for path in unique_paths
            }
        return {path: remote[resolved_by_path[path]] for path in unique_paths}

    def list_folder_files(self, folder: str) -> list[str]:
        """Return every regular file under *folder* as absolute paths."""
        resolved_folder = self._resolve_path(folder)
        if self._sandbox is None or getattr(self._sandbox, "process", None) is None:
            root = Path(resolved_folder)
            if not root.exists():
                raise FileNotFoundError(folder)
            if not root.is_dir():
                raise NotADirectoryError(folder)
            return sorted(str(path) for path in root.rglob("*") if path.is_file())
        return self._list_remote_folder_files(resolved_folder)

    def write(self, file_path: str, content: str) -> None:
        """Write *content* to *file_path*, preferring the sandbox when bound."""
        resolved_path = self._resolve_path(file_path)
        if self._sandbox is None:
            path = Path(resolved_path)
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(content, encoding="utf-8")
            return
        if getattr(self._sandbox, "process", None) is None:
            fs = getattr(self._sandbox, "fs", None)
            if fs is not None and callable(getattr(fs, "upload_file", None)):
                run_sync(fs.upload_file(content.encode("utf-8"), resolved_path))
                return
            path = Path(resolved_path)
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(content, encoding="utf-8")
            return
        self._write_remote(resolved_path, content.encode("utf-8"))

    def delete(self, file_path: str) -> None:
        """Delete *file_path*, preferring the sandbox when one is bound."""
        resolved_path = self._resolve_path(file_path)
        if self._sandbox is None:
            path = Path(resolved_path)
            try:
                path.unlink()
            except FileNotFoundError:
                return
            return
        if getattr(self._sandbox, "process", None) is None:
            fs = getattr(self._sandbox, "fs", None)
            delete_fn = getattr(fs, "delete_file", None) if fs is not None else None
            if callable(delete_fn):
                run_sync(delete_fn(resolved_path))
                return
            path = Path(resolved_path)
            try:
                path.unlink()
            except FileNotFoundError:
                return
            return
        self._delete_remote(resolved_path)

    def apply_many(self, changes: list[tuple[str, str | None]]) -> None:
        """Apply many writes/deletes through one sandbox round trip when possible."""
        if not changes:
            return
        if self._sandbox is None:
            for file_path, content in changes:
                if content is None:
                    self.delete(file_path)
                else:
                    self.write(file_path, content)
            return
        if getattr(self._sandbox, "process", None) is None:
            for file_path, content in changes:
                if content is None:
                    self.delete(file_path)
                else:
                    self.write(file_path, content)
            return
        self._apply_remote_batch(changes)

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
        if self._sandbox is None:
            return self._apply_local_batch_checked(changes)
        if getattr(self._sandbox, "process", None) is None:
            return CheckedApplyResult(
                success=False,
                conflict_reason="unsupported",
                message="checked apply requires process-backed sandbox",
            )
        return self._apply_remote_batch_checked(changes)

    # -- Private --------------------------------------------------------------

    def _resolve_path(self, file_path: str) -> str:
        return resolve_workspace_path(file_path, self._workspace_root)

    def _process(self) -> Any:
        process = getattr(self._sandbox, "process", None)
        if process is None:
            raise RuntimeError("Sandbox process is unavailable")
        return process

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
        if not callable(download_files_fn) or not _is_real_daytona_fs(fs):
            return None
        try:
            from daytona_sdk.common.filesystem import FileDownloadRequest
        except ImportError:
            return None

        try:
            requests = [FileDownloadRequest(source=path) for path in file_paths]
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

    def _read_remote(self, file_path: str, *, allow_missing: bool) -> FileReadResult:
        process = self._process()
        response = run_sync(
            process.exec(_wrap_bash_command(_build_read_text_file_command(file_path)))
        )
        cleaned, exit_code = _extract_exit_code(
            getattr(response, "result", "") or "",
            fallback_exit_code=getattr(response, "exit_code", None),
        )
        if exit_code not in (0, None):
            raise RuntimeError(cleaned or f"read failed for {file_path}")
        payload = json.loads(cleaned or "{}")
        if not payload.get("exists"):
            if allow_missing:
                return "", False
            raise FileNotFoundError(file_path)
        return str(payload.get("content", "") or ""), True

    def _read_remote_batch(
        self,
        file_paths: list[str],
        *,
        allow_missing: bool,
    ) -> FileReadResults:
        process = self._process()
        script = """
import json
import pathlib
import sys

files = {}
for raw_path in sys.argv[1:]:
    path = pathlib.Path(raw_path)
    try:
        content = path.read_text(encoding="utf-8")
    except FileNotFoundError:
        files[raw_path] = {"exists": False, "content": ""}
    else:
        files[raw_path] = {"exists": True, "content": content}
print(json.dumps(files))
"""
        command = f"python3 -c {shlex.quote(script)} " + " ".join(
            shlex.quote(path) for path in file_paths
        )
        response = run_sync(process.exec(_wrap_bash_command(command)))
        cleaned, exit_code = _extract_exit_code(
            getattr(response, "result", "") or "",
            fallback_exit_code=getattr(response, "exit_code", None),
        )
        if exit_code not in (0, None):
            raise RuntimeError(cleaned or "batch read failed")
        payload = json.loads(cleaned or "{}")
        results: FileReadResults = {}
        for path in file_paths:
            item = payload.get(path) if isinstance(payload, dict) else None
            if not isinstance(item, dict) or not item.get("exists"):
                if allow_missing:
                    results[path] = ("", False)
                    continue
                raise FileNotFoundError(path)
            results[path] = (str(item.get("content", "") or ""), True)
        return results

    def _list_remote_folder_files(self, folder: str) -> list[str]:
        process = self._process()
        probe_cmd = (
            f"if [ ! -e {shlex.quote(folder)} ]; then echo __MISSING__; "
            f"elif [ ! -d {shlex.quote(folder)} ]; then echo __NOTDIR__; "
            f"else find {shlex.quote(folder)} -type f -print; fi"
        )
        response = run_sync(process.exec(_wrap_bash_command(probe_cmd)))
        cleaned, exit_code = _extract_exit_code(
            getattr(response, "result", "") or "",
            fallback_exit_code=getattr(response, "exit_code", None),
        )
        if exit_code not in (0, None):
            raise RuntimeError(cleaned or f"enumerate failed for {folder}")
        lines = [line for line in cleaned.splitlines() if line.strip()]
        if lines and lines[0].strip() == "__MISSING__":
            raise FileNotFoundError(folder)
        if lines and lines[0].strip() == "__NOTDIR__":
            raise NotADirectoryError(folder)
        return lines

    def _write_remote(self, file_path: str, payload: bytes) -> None:
        process = self._process()
        text = payload.decode("utf-8")
        commands, tmp_path = _build_write_text_file_commands(file_path, text)
        try:
            for command in commands:
                response = run_sync(process.exec(_wrap_bash_command(command)))
                cleaned, exit_code = _extract_exit_code(
                    getattr(response, "result", "") or "",
                    fallback_exit_code=getattr(response, "exit_code", None),
                )
                if exit_code not in (0, None):
                    raise RuntimeError(cleaned or f"write failed for {file_path}")
        except Exception:
            if tmp_path:
                try:
                    run_sync(process.exec(_wrap_bash_command(_build_remove_file_command(tmp_path))))
                except Exception:
                    logger.debug("remote temp cleanup failed for %s", tmp_path, exc_info=True)
            raise

    def _delete_remote(self, file_path: str) -> None:
        process = self._process()
        response = run_sync(process.exec(_wrap_bash_command(f"rm -f {shlex.quote(file_path)}")))
        cleaned, exit_code = _extract_exit_code(
            getattr(response, "result", "") or "",
            fallback_exit_code=getattr(response, "exit_code", None),
        )
        if exit_code not in (0, None):
            raise RuntimeError(cleaned or f"delete failed for {file_path}")

    def _exec_remote(self, process: Any, command: str) -> tuple[str, int | None]:
        """Run *command* through the sandbox and return ``(cleaned_stdout, exit_code)``."""
        response = run_sync(process.exec(_wrap_bash_command(command)))
        return _extract_exit_code(
            getattr(response, "result", "") or "",
            fallback_exit_code=getattr(response, "exit_code", None),
        )

    def _cleanup_remote_tmp(self, process: Any, tmp_path: str) -> None:
        try:
            run_sync(
                process.exec(
                    _wrap_bash_command(_build_remove_file_command(tmp_path)),
                ),
            )
        except Exception:
            logger.debug(
                "remote batch tmp cleanup failed for %s",
                tmp_path,
                exc_info=True,
            )

    def _stage_remote_payload(self, process: Any, payload: bytes) -> str:
        """Write *payload* to a unique remote tmp file via chunked base64 appends.

        Returns the tmp path. Used to pass large batch payloads to apply scripts
        without inlining them into the command line (which trips ARG_MAX/E2BIG).
        Caller is responsible for removing the tmp file via ``_cleanup_remote_tmp``.
        """
        tmp_path = f"/tmp/codex-batch-apply-{uuid.uuid4().hex}.json"
        cleaned, exit_code = self._exec_remote(
            process, _build_truncate_text_file_command(tmp_path),
        )
        if exit_code not in (0, None):
            raise RuntimeError(cleaned or "stage payload truncate failed")
        chunk_size = _REMOTE_WRITE_CHUNK_BYTES
        for index in range(0, len(payload), chunk_size):
            chunk = payload[index : index + chunk_size]
            chunk_b64 = base64.b64encode(chunk).decode("ascii")
            cleaned, exit_code = self._exec_remote(
                process,
                _build_append_text_file_chunk_command(tmp_path, chunk_b64),
            )
            if exit_code not in (0, None):
                self._cleanup_remote_tmp(process, tmp_path)
                raise RuntimeError(cleaned or "stage payload chunk failed")
        return tmp_path

    def _apply_remote_batch(self, changes: list[tuple[str, str | None]]) -> None:
        process = self._process()
        payload = [
            {
                "path": self._resolve_path(path),
                "content_b64": (
                    None
                    if content is None
                    else base64.b64encode(content.encode("utf-8")).decode("ascii")
                ),
            }
            for path, content in changes
        ]
        payload_bytes = json.dumps(payload).encode("utf-8")
        if len(payload_bytes) > _REMOTE_WRITE_CHUNK_BYTES:
            self._apply_remote_batch_staged(process, payload_bytes)
            return
        script = _build_inline_apply_script(payload_bytes, _APPLY_BODY)
        command = f"python3 -c {shlex.quote(script)}"
        cleaned, exit_code = self._exec_remote(process, command)
        if exit_code not in (0, None):
            raise RuntimeError(cleaned or "batch apply failed")

    def _apply_remote_batch_staged(self, process: Any, payload_bytes: bytes) -> None:
        tmp_path = self._stage_remote_payload(process, payload_bytes)
        try:
            command = (
                f"python3 -c {shlex.quote(_BATCH_APPLY_FROM_FILE_SCRIPT)} "
                f"{shlex.quote(tmp_path)}"
            )
            cleaned, exit_code = self._exec_remote(process, command)
            if exit_code not in (0, None):
                raise RuntimeError(cleaned or "batch apply failed")
        finally:
            self._cleanup_remote_tmp(process, tmp_path)

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

    def _apply_remote_batch_checked(
        self,
        changes: list[CheckedApplyChange],
    ) -> CheckedApplyResult:
        process = self._process()
        payload = [
            {
                "original_path": change.file_path,
                "path": self._resolve_path(change.file_path),
                "base_hash": change.base_hash,
                "base_existed": change.base_existed,
                "content_b64": (
                    None
                    if change.final_content is None
                    else base64.b64encode(
                        change.final_content.encode("utf-8"),
                    ).decode("ascii")
                ),
            }
            for change in changes
        ]
        payload_bytes = json.dumps(payload).encode("utf-8")
        if len(payload_bytes) > _REMOTE_WRITE_CHUNK_BYTES:
            return self._apply_remote_batch_checked_staged(process, payload_bytes)
        script = _build_inline_apply_script(payload_bytes, _CHECKED_APPLY_BODY)
        command = f"python3 -c {shlex.quote(script)}"
        cleaned, exit_code = self._exec_remote(process, command)
        return _parse_checked_apply_response(cleaned, exit_code)

    def _apply_remote_batch_checked_staged(
        self,
        process: Any,
        payload_bytes: bytes,
    ) -> CheckedApplyResult:
        tmp_path = self._stage_remote_payload(process, payload_bytes)
        try:
            command = (
                f"python3 -c {shlex.quote(_CHECKED_BATCH_APPLY_FROM_FILE_SCRIPT)} "
                f"{shlex.quote(tmp_path)}"
            )
            cleaned, exit_code = self._exec_remote(process, command)
            return _parse_checked_apply_response(cleaned, exit_code)
        finally:
            self._cleanup_remote_tmp(process, tmp_path)
