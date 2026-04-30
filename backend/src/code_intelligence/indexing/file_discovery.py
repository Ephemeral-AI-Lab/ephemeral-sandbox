"""File discovery and sandbox I/O for the symbol index."""

from __future__ import annotations

import json
import logging
import posixpath
import shlex
from pathlib import Path
from typing import Any

from sandbox.daytona_utils import _extract_exit_code, _wrap_bash_command

from sandbox.async_bridge import run_sync
from code_intelligence.core.constants import SKIP_DIRECTORIES, SUPPORTED_EXTENSIONS

logger = logging.getLogger(__name__)

_REMOTE_GLOB = "*"


def collect_local_files(root: Path, max_files: int) -> list[Path]:
    """Walk *root* collecting indexable files (bounded by *max_files*)."""
    files: list[Path] = []
    for path in root.rglob("*"):
        if len(files) >= max_files:
            break
        if any(part in SKIP_DIRECTORIES for part in path.parts):
            continue
        if path.is_file() and path.suffix in SUPPORTED_EXTENSIONS:
            files.append(path)
    files.sort()
    return files


def collect_remote_files(sandbox: Any, root: str, max_files: int) -> list[str] | None:
    """Enumerate sandbox files, preferring the single-call glob API."""
    fs = getattr(sandbox, "fs", None) if sandbox is not None else None
    if fs is None:
        return None
    via_search = _collect_via_search(fs, root, max_files)
    if via_search is not None:
        return via_search
    return _collect_via_list(fs, root, max_files)


def read_file_content(file_path: str, sandbox: Any = None) -> str | None:
    """Read a file locally; fall back to a sandbox download."""
    try:
        return Path(file_path).read_text(encoding="utf-8")
    except Exception:
        pass

    content = _read_text_via_exec(sandbox, file_path)
    if content is not None:
        return content

    fs = getattr(sandbox, "fs", None) if sandbox is not None else None
    download = getattr(fs, "download_file", None)
    if not callable(download):
        return None
    try:
        raw = run_sync(download(file_path))
    except Exception:
        logger.debug("Remote download_file failed for %s", file_path, exc_info=True)
        return None
    if isinstance(raw, bytes):
        return raw.decode("utf-8")
    return str(raw)


def batch_download(
    sandbox: Any,
    files: list[str],
) -> list[tuple[str, str]] | None:
    """Download *files* from the sandbox in one multipart request.

    Returns a list of ``(file_path, content)`` tuples, or ``None`` when the
    sandbox does not expose the batch API. Individual download failures are
    silently dropped.
    """
    via_exec = _batch_read_text_via_exec(sandbox, files)
    if via_exec is not None:
        return via_exec

    fs = getattr(sandbox, "fs", None) if sandbox is not None else None
    download_files_fn = getattr(fs, "download_files", None)
    if not callable(download_files_fn) or not _is_real_sdk(fs):
        return None

    try:
        from daytona_sdk.common.filesystem import FileDownloadRequest
    except ImportError:
        return None

    try:
        requests = [FileDownloadRequest(source=fp) for fp in files]
        responses = run_sync(download_files_fn(requests))
    except Exception:
        logger.debug("Batch download_files failed", exc_info=True)
        return None

    out: list[tuple[str, str]] = []
    for resp in responses or []:
        fp = getattr(resp, "source", None)
        if fp is None or getattr(resp, "error", None) or getattr(resp, "result", None) is None:
            continue
        raw = resp.result
        content = raw.decode("utf-8") if isinstance(raw, bytes) else str(raw)
        out.append((fp, content))
    return out


# -- Remote enumeration helpers ----------------------------------------------


def _collect_via_search(fs: Any, root: str, max_files: int) -> list[str] | None:
    search_fn = getattr(fs, "search_files", None)
    if not callable(search_fn) or not _is_real_sdk(fs):
        return None
    try:
        result = run_sync(search_fn(root, _REMOTE_GLOB))
        raw_files = getattr(result, "files", None) or []
    except Exception as exc:
        reason = str(exc).splitlines()[0] if str(exc) else type(exc).__name__
        logger.debug("search_files failed, falling back to list_files: %s", reason)
        return None

    files: list[str] = []
    for fp in raw_files:
        if len(files) >= max_files:
            break
        if not isinstance(fp, str):
            continue
        parts = Path(fp).parts
        if any(part in SKIP_DIRECTORIES for part in parts):
            continue
        if Path(fp).suffix.lower() in SUPPORTED_EXTENSIONS:
            files.append(fp)
    files.sort()
    return files


def _collect_via_list(fs: Any, root: str, max_files: int) -> list[str] | None:
    list_files_fn = getattr(fs, "list_files", None)
    if not callable(list_files_fn):
        return None

    files: list[str] = []
    pending: list[str] = [str(root).rstrip("/") or "/"]

    while pending and len(files) < max_files:
        current = pending.pop()
        try:
            entries = run_sync(list_files_fn(current)) or []
        except Exception:
            logger.debug("Remote list_files failed for %s", current, exc_info=True)
            return None

        for entry in entries:
            if len(files) >= max_files:
                break
            name = getattr(entry, "name", None)
            if not isinstance(name, str) or not name or name in {".", ".."}:
                continue
            child = posixpath.join(current, name)
            parts = Path(child).parts
            if any(part in SKIP_DIRECTORIES for part in parts):
                continue
            if bool(getattr(entry, "is_dir", False)):
                pending.append(child)
                continue
            if Path(child).suffix.lower() in SUPPORTED_EXTENSIONS:
                files.append(child)

    files.sort()
    return files


def _is_real_sdk(fs: Any) -> bool:
    """Best-effort check that *fs* is the Daytona SDK, not a MagicMock."""
    mod = getattr(type(fs), "__module__", "") or ""
    return "daytona" in mod


def _supports_exec_transport(sandbox: Any) -> bool:
    process = getattr(sandbox, "process", None) if sandbox is not None else None
    exec_fn = getattr(process, "exec", None) if process is not None else None
    if not callable(exec_fn):
        return False
    return not getattr(type(exec_fn), "__module__", "").startswith("unittest.mock")


def _read_text_via_exec(sandbox: Any, file_path: str) -> str | None:
    if not _supports_exec_transport(sandbox):
        return None
    script = """
import json
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
try:
    print(path.read_text(encoding="utf-8"))
except Exception as exc:
    print(json.dumps({"error": str(exc)}))
    raise
"""
    try:
        response = run_sync(
            sandbox.process.exec(
                _wrap_bash_command(
                    f"python3 -c {shlex.quote(script)} {shlex.quote(file_path)}"
                )
            )
        )
        stdout, exit_code = _extract_exit_code(
            getattr(response, "result", "") or "",
            fallback_exit_code=getattr(response, "exit_code", None),
        )
        if exit_code not in (0, None):
            return None
        return stdout
    except Exception:
        logger.debug("process.exec read failed for %s", file_path, exc_info=True)
        return None


def _batch_read_text_via_exec(
    sandbox: Any,
    files: list[str],
) -> list[tuple[str, str]] | None:
    if not files or not _supports_exec_transport(sandbox):
        return None
    script = """
import json
import pathlib
import sys

payload = []
for raw_path in sys.argv[1:]:
    path = pathlib.Path(raw_path)
    try:
        payload.append({"path": raw_path, "content": path.read_text(encoding="utf-8")})
    except Exception:
        continue
print(json.dumps(payload))
"""
    args = " ".join(shlex.quote(path) for path in files)
    try:
        response = run_sync(
            sandbox.process.exec(
                _wrap_bash_command(f"python3 -c {shlex.quote(script)} {args}")
            )
        )
        stdout, exit_code = _extract_exit_code(
            getattr(response, "result", "") or "",
            fallback_exit_code=getattr(response, "exit_code", None),
        )
        if exit_code not in (0, None):
            return None
        payload = json.loads(stdout or "[]")
        out: list[tuple[str, str]] = []
        for item in payload:
            if not isinstance(item, dict):
                continue
            fp = item.get("path")
            content = item.get("content")
            if isinstance(fp, str) and isinstance(content, str):
                out.append((fp, content))
        return out
    except Exception:
        logger.debug("process.exec batch read failed", exc_info=True)
        return None
