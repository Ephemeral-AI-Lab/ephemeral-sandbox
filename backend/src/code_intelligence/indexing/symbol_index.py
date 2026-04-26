"""Background symbol indexing for a workspace.

Owns only indexing state + build coordination. Extraction is delegated
to :mod:`code_intelligence.indexing.symbol_extractor` and file discovery
to :mod:`code_intelligence.indexing.file_discovery`.
"""

from __future__ import annotations

import concurrent.futures
import logging
import threading
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from code_intelligence.indexing.file_discovery import (
    batch_download,
    collect_local_files,
    collect_remote_files,
    read_file_content,
)
from code_intelligence.indexing.symbol_extractor import extract_symbols
from code_intelligence.core.constants import (
    SYMBOL_INDEX_BATCH_SIZE,
    SYMBOL_INDEX_MAX_FILES,
)
from code_intelligence.core.types import SymbolInfo, SymbolKind

logger = logging.getLogger(__name__)

_REMOTE_THREAD_POOL = 8
_REMOTE_BATCH_SIZE = 50


@dataclass
class _FileSymbols:
    """Symbols extracted from a single file."""

    file_path: str
    symbols: list[SymbolInfo] = field(default_factory=list)
    generation: int = 0
    indexed_at: float = 0.0


class SymbolIndex:
    """Thread-safe workspace symbol index with lazy background builds."""

    def __init__(
        self,
        workspace_root: str,
        max_files: int = SYMBOL_INDEX_MAX_FILES,
        sandbox: Any = None,
    ) -> None:
        self._workspace_root = workspace_root
        self._max_files = max_files
        self._sandbox = sandbox

        self._lock = threading.Lock()
        self._symbols: dict[str, _FileSymbols] = {}
        self._built = False
        self._building = False
        self._build_event = threading.Event()
        self._generation = 0
        self._build_thread: threading.Thread | None = None

    # -- Public API -----------------------------------------------------------

    def ensure_built(self, wait: bool = True, timeout: float = 30.0) -> bool:
        """Trigger a background build if not already built. Returns readiness."""
        with self._lock:
            if self._built:
                return True
            if not self._building:
                self._start_build()
        if wait:
            self._build_event.wait(timeout=timeout)
            with self._lock:
                return self._built
        return False

    def refresh(self, file_path: str, content: str | None = None) -> int:
        """Re-index a single file. Returns the new generation."""
        if content is None:
            content = read_file_content(file_path, self._sandbox)
            if content is None:
                return self.remove(file_path)
        symbols = extract_symbols(file_path, content)
        with self._lock:
            self._generation += 1
            gen = self._generation
            self._symbols[file_path] = _FileSymbols(
                file_path=file_path,
                symbols=symbols,
                generation=gen,
                indexed_at=time.time(),
            )
        return gen

    def remove(self, file_path: str) -> int:
        """Remove a file from the index. Returns the new generation."""
        with self._lock:
            if file_path not in self._symbols:
                return self._generation
            self._generation += 1
            del self._symbols[file_path]
            return self._generation

    def find(self, query: str, kind: SymbolKind | None = None) -> list[SymbolInfo]:
        """Find symbols matching a query string (case-insensitive substring)."""
        needle = query.lower().strip()
        if not needle:
            return []
        results: list[SymbolInfo] = []
        with self._lock:
            for fs in self._symbols.values():
                for sym in fs.symbols:
                    if needle in sym.name.lower() and (kind is None or sym.kind == kind):
                        results.append(sym)
        return results

    def file_symbols(self, file_path: str) -> list[SymbolInfo]:
        """Return all symbols in a specific file."""
        candidates = [str(file_path or "")]
        root = str(self._workspace_root or "").rstrip("/\\")
        normalized = candidates[0].replace("\\", "/")
        if root:
            root_prefix = root.replace("\\", "/").rstrip("/")
            if normalized.startswith(root_prefix + "/"):
                candidates.append(normalized[len(root_prefix) + 1 :])
            elif normalized and not normalized.startswith("/"):
                candidates.append(f"{root_prefix}/{normalized}")
        with self._lock:
            for candidate in candidates:
                fs = self._symbols.get(candidate)
                if fs:
                    return list(fs.symbols)
            return []

    def indexed_paths(self) -> list[str]:
        """Return all indexed file paths, sorted."""
        with self._lock:
            return sorted(self._symbols.keys())

    def paths_with_prefix(self, prefix: str = "") -> list[str]:
        """Return indexed paths beginning with *prefix* (workspace-relative match)."""
        from code_intelligence.core.path_utils import relativize_workspace_path

        root = self._workspace_root
        normalized_prefix = relativize_workspace_path(prefix, workspace_root=root)
        with self._lock:
            paths = sorted(self._symbols.keys())
        if not normalized_prefix:
            return paths
        prefix_with_slash = normalized_prefix.rstrip("/") + "/"
        out: list[str] = []
        for path in paths:
            rel = relativize_workspace_path(path, workspace_root=root)
            if rel == normalized_prefix or rel.startswith(prefix_with_slash):
                out.append(path)
        return out

    @property
    def is_built(self) -> bool:
        with self._lock:
            return self._built

    @property
    def generation(self) -> int:
        with self._lock:
            return self._generation

    @property
    def size(self) -> int:
        with self._lock:
            return sum(len(fs.symbols) for fs in self._symbols.values())

    @property
    def indexed_files(self) -> int:
        with self._lock:
            return len(self._symbols)

    def bind_sandbox(self, sandbox: Any) -> None:
        """Update the sandbox used for remote file access."""
        self._sandbox = sandbox

    # -- Background build -----------------------------------------------------

    def _start_build(self) -> None:
        """Start a background build thread (must hold ``_lock``)."""
        self._building = True
        self._build_event.clear()
        self._build_thread = threading.Thread(
            target=self._background_build,
            name="symbol-index-build",
            daemon=True,
        )
        self._build_thread.start()

    def _background_build(self) -> None:
        """Index all files in the workspace."""
        try:
            files = self._discover_files()
            if files is None:
                logger.warning("Workspace root does not exist: %s", self._workspace_root)
                self._finish_build(built=False)
                return

            logger.info(
                "Symbol index: building for %d files in %s",
                len(files),
                self._workspace_root,
            )

            if self._sandbox is not None:
                self._build_remote(files)
            else:
                self._build_sequential(files)

            self._finish_build(built=True)
            logger.info(
                "Symbol index: built (%d files, %d symbols)",
                self.indexed_files,
                self.size,
            )

        except Exception:
            logger.exception("Symbol index build failed")
            self._finish_build(built=False)

    def _finish_build(self, *, built: bool) -> None:
        with self._lock:
            self._built = built
            self._building = False
            self._build_event.set()

    def _discover_files(self) -> list[str] | None:
        root = Path(self._workspace_root)
        if root.is_dir():
            return [str(fp) for fp in collect_local_files(root, self._max_files)]
        return collect_remote_files(self._sandbox, self._workspace_root, self._max_files)

    def _build_sequential(self, files: list[str]) -> None:
        """Index files one at a time (local filesystem)."""
        batch: list[tuple[str, list[SymbolInfo]]] = []
        for file_path in files:
            content = read_file_content(file_path, self._sandbox)
            if content is None:
                continue
            batch.append((file_path, extract_symbols(file_path, content)))
            if len(batch) >= SYMBOL_INDEX_BATCH_SIZE:
                self._commit_batch(batch)
                batch.clear()
        if batch:
            self._commit_batch(batch)

    def _build_remote(self, files: list[str]) -> None:
        """Download remote files (batch API preferred, thread-pool fallback)."""
        for i in range(0, len(files), _REMOTE_BATCH_SIZE):
            chunk = files[i : i + _REMOTE_BATCH_SIZE]
            downloaded = batch_download(self._sandbox, chunk)
            if downloaded is None:
                self._build_remote_individual(chunk)
                continue
            commit = [(fp, extract_symbols(fp, content)) for fp, content in downloaded]
            if commit:
                self._commit_batch(commit)

    def _build_remote_individual(self, files: list[str]) -> None:
        """Fallback: download files individually using a thread pool."""

        def _download_and_extract(fp: str) -> tuple[str, list[SymbolInfo]]:
            content = read_file_content(fp, self._sandbox)
            symbols = extract_symbols(fp, content) if content else []
            return fp, symbols

        batch: list[tuple[str, list[SymbolInfo]]] = []
        with concurrent.futures.ThreadPoolExecutor(max_workers=_REMOTE_THREAD_POOL) as pool:
            for result in pool.map(_download_and_extract, files):
                batch.append(result)
                if len(batch) >= SYMBOL_INDEX_BATCH_SIZE:
                    self._commit_batch(batch)
                    batch.clear()
        if batch:
            self._commit_batch(batch)

    def _commit_batch(self, batch: list[tuple[str, list[SymbolInfo]]]) -> None:
        """Commit a batch of indexed files under a single generation bump."""
        with self._lock:
            self._generation += 1
            gen = self._generation
            for fp, symbols in batch:
                self._symbols[fp] = _FileSymbols(
                    file_path=fp,
                    symbols=symbols,
                    generation=gen,
                    indexed_at=time.time(),
                )
