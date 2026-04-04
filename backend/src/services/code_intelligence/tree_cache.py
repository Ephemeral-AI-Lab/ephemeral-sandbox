"""TreeCache — thread-safe AST caching with two-tier validation.

Uses mtime fast-path and content-hash fallback. Falls back to Python's
``ast`` module when tree-sitter is unavailable.

Lock ordering (Group A):
    A3: per-file locks  <  A4: dict lock  <  A5: counter lock
"""

from __future__ import annotations

import ast
import hashlib
import logging
import threading
import time
from collections import OrderedDict
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Callable

from ephemeralos.services.code_intelligence.constants import (
    SKIP_DIRECTORIES,
    TREE_CACHE_MAX_FILE_SIZE,
    TREE_CACHE_MAX_FILES,
)

logger = logging.getLogger(__name__)

# Try to import tree-sitter; fall back gracefully
try:
    import tree_sitter  # type: ignore[import-untyped]
    _HAS_TREE_SITTER = True
except ImportError:
    tree_sitter = None  # type: ignore[assignment]
    _HAS_TREE_SITTER = False


def _content_hash(content: str) -> str:
    return hashlib.sha256(content.encode("utf-8")).hexdigest()[:16]


@dataclass
class CacheEntry:
    """A cached parse result for a single file."""

    file_path: str
    content: str
    content_hash: str
    tree: Any  # tree-sitter Tree or ast.Module
    language: str
    mtime: float
    parsed_at: float = field(default_factory=time.time)


class TreeCache:
    """Thread-safe AST cache with LRU eviction.

    Parameters
    ----------
    max_files:
        Maximum cached files before LRU eviction.
    max_file_size:
        Skip files larger than this (bytes).
    on_change:
        Optional callback ``(file_path, old_hash, new_hash)`` fired on cache update.
    """

    def __init__(
        self,
        max_files: int = TREE_CACHE_MAX_FILES,
        max_file_size: int = TREE_CACHE_MAX_FILE_SIZE,
        on_change: Callable[[str, str, str], None] | None = None,
    ) -> None:
        self._max_files = max_files
        self._max_file_size = max_file_size
        self._on_change = on_change

        # Group A locks
        self._file_locks: dict[str, threading.Lock] = {}  # A3
        self._dict_lock = threading.Lock()  # A4
        self._counter_lock = threading.Lock()  # A5

        self._cache: OrderedDict[str, CacheEntry] = OrderedDict()
        self._hits = 0
        self._misses = 0

    # -- Public API -----------------------------------------------------------

    def get_tree(
        self,
        file_path: str,
        content: str | None = None,
        mtime: float | None = None,
    ) -> CacheEntry | None:
        """Get or parse a file's AST.

        If *content* is provided, parse it directly. Otherwise try to
        read from the filesystem. Returns ``None`` on parse failure.
        """
        file_lock = self._get_file_lock(file_path)
        with file_lock:
            # Check cache
            with self._dict_lock:
                entry = self._cache.get(file_path)

            if entry is not None and content is None:
                # mtime fast path
                if mtime is not None and entry.mtime == mtime:
                    self._record_hit()
                    with self._dict_lock:
                        self._cache.move_to_end(file_path)
                    return entry
                # content-hash fallback
                if mtime is None:
                    self._record_hit()
                    with self._dict_lock:
                        self._cache.move_to_end(file_path)
                    return entry

            # Need to parse
            if content is None:
                try:
                    p = Path(file_path)
                    if not p.is_file() or p.stat().st_size > self._max_file_size:
                        return None
                    content = p.read_text(encoding="utf-8")
                    mtime = p.stat().st_mtime
                except Exception:
                    return None

            new_hash = _content_hash(content)

            # If cached and hash matches, update mtime only
            if entry is not None and entry.content_hash == new_hash:
                entry = CacheEntry(
                    file_path=file_path,
                    content=content,
                    content_hash=new_hash,
                    tree=entry.tree,
                    language=entry.language,
                    mtime=mtime or 0.0,
                )
                with self._dict_lock:
                    self._cache[file_path] = entry
                    self._cache.move_to_end(file_path)
                self._record_hit()
                return entry

            # Parse
            self._record_miss()
            language = self._detect_language(file_path)
            tree = self._parse(content, language)
            if tree is None:
                return None

            old_hash = entry.content_hash if entry else ""
            new_entry = CacheEntry(
                file_path=file_path,
                content=content,
                content_hash=new_hash,
                tree=tree,
                language=language,
                mtime=mtime or 0.0,
            )

            with self._dict_lock:
                self._cache[file_path] = new_entry
                self._cache.move_to_end(file_path)
                self._evict_if_needed()

            if self._on_change and old_hash and old_hash != new_hash:
                try:
                    self._on_change(file_path, old_hash, new_hash)
                except Exception:
                    logger.debug("on_change callback failed for %s", file_path)

            return new_entry

    def put_content(self, file_path: str, content: str) -> CacheEntry | None:
        """Parse and cache content for a file (no disk read)."""
        return self.get_tree(file_path, content=content)

    def prime_cache(self, file_paths: list[str]) -> int:
        """Pre-parse a list of files. Returns count of successfully cached files."""
        count = 0
        for fp in file_paths:
            if self.get_tree(fp) is not None:
                count += 1
        return count

    def invalidate(self, file_path: str) -> None:
        """Remove a file from the cache."""
        with self._dict_lock:
            self._cache.pop(file_path, None)

    def invalidate_files(self, file_paths: list[str]) -> None:
        """Remove multiple files from the cache."""
        with self._dict_lock:
            for fp in file_paths:
                self._cache.pop(fp, None)

    def invalidate_all(self) -> None:
        """Clear the entire cache."""
        with self._dict_lock:
            self._cache.clear()

    def invalidate_prefix(self, prefix: str) -> int:
        """Invalidate all files under a path prefix. Returns count removed."""
        with self._dict_lock:
            to_remove = [k for k in self._cache if k.startswith(prefix)]
            for k in to_remove:
                del self._cache[k]
            return len(to_remove)

    @property
    def size(self) -> int:
        with self._dict_lock:
            return len(self._cache)

    @property
    def stats(self) -> dict[str, int]:
        with self._counter_lock:
            return {"size": self.size, "hits": self._hits, "misses": self._misses}

    # -- Internal -------------------------------------------------------------

    def _get_file_lock(self, file_path: str) -> threading.Lock:
        """Get or create a per-file lock (Group A3)."""
        with self._dict_lock:
            if file_path not in self._file_locks:
                self._file_locks[file_path] = threading.Lock()
            return self._file_locks[file_path]

    def _evict_if_needed(self) -> None:
        """LRU eviction (must hold _dict_lock)."""
        while len(self._cache) > self._max_files:
            self._cache.popitem(last=False)

    def _record_hit(self) -> None:
        with self._counter_lock:
            self._hits += 1

    def _record_miss(self) -> None:
        with self._counter_lock:
            self._misses += 1

    def _detect_language(self, file_path: str) -> str:
        """Detect language from file extension."""
        ext = Path(file_path).suffix.lower()
        mapping = {
            ".py": "python",
            ".js": "javascript",
            ".jsx": "javascript",
            ".ts": "typescript",
            ".tsx": "typescript",
            ".java": "java",
            ".go": "go",
            ".rs": "rust",
            ".rb": "ruby",
            ".c": "c",
            ".cpp": "cpp",
            ".h": "c",
            ".hpp": "cpp",
        }
        return mapping.get(ext, "unknown")

    def _parse(self, content: str, language: str) -> Any:
        """Parse content into an AST. Falls back to Python ast if tree-sitter unavailable."""
        if _HAS_TREE_SITTER:
            return self._parse_tree_sitter(content, language)
        if language == "python":
            return self._parse_python_ast(content)
        # For non-Python without tree-sitter, store raw content
        return content

    def _parse_tree_sitter(self, content: str, language: str) -> Any:
        """Parse using tree-sitter."""
        try:
            # tree-sitter language loading varies by version
            lang = tree_sitter.Language(f"tree-sitter-{language}")  # type: ignore[arg-type]
            parser = tree_sitter.Parser()
            parser.set_language(lang)
            return parser.parse(content.encode("utf-8"))
        except Exception:
            # Fall back to Python ast for .py files
            if language == "python":
                return self._parse_python_ast(content)
            return content

    def _parse_python_ast(self, content: str) -> ast.Module | None:
        """Parse Python source with the stdlib ast module."""
        try:
            return ast.parse(content)
        except SyntaxError:
            return None
