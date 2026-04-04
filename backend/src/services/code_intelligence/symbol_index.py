"""SymbolIndex — per-sandbox symbol indexing with background builds.

Provides lazy-build background indexing with per-file refresh,
generational tracking, and per-file completion events to avoid
O(N) spurious wakeups.
"""

from __future__ import annotations

import ast
import logging
import threading
import time
from collections import defaultdict
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from ephemeralos.services.code_intelligence.constants import (
    SKIP_DIRECTORIES,
    SUPPORTED_EXTENSIONS,
    SYMBOL_INDEX_BATCH_SIZE,
    SYMBOL_INDEX_MAX_FILES,
)
from ephemeralos.services.code_intelligence.types import SymbolInfo, SymbolKind

logger = logging.getLogger(__name__)


@dataclass
class _FileSymbols:
    """Symbols extracted from a single file."""

    file_path: str
    symbols: list[SymbolInfo] = field(default_factory=list)
    generation: int = 0
    indexed_at: float = 0.0


class SymbolIndex:
    """Per-workspace symbol index with background building.

    Thread-safe. Supports lazy builds triggered by ``ensure_built()``,
    per-file refresh via ``refresh()``, and generational tracking.

    Parameters
    ----------
    workspace_root:
        Root directory to index.
    max_files:
        Maximum files to index (prevents unbounded memory growth).
    """

    def __init__(
        self,
        workspace_root: str,
        max_files: int = SYMBOL_INDEX_MAX_FILES,
    ) -> None:
        self._workspace_root = workspace_root
        self._max_files = max_files

        # Index state
        self._lock = threading.Lock()
        self._symbols: dict[str, _FileSymbols] = {}
        self._built = False
        self._building = False
        self._build_event = threading.Event()
        self._generation = 0

        # Per-file waiters
        self._file_events: dict[str, threading.Event] = {}

        # Background thread
        self._build_thread: threading.Thread | None = None
        self._pending_rebuild = False

    # -- Public API -----------------------------------------------------------

    def ensure_built(self, wait: bool = True, timeout: float = 30.0) -> bool:
        """Trigger a background build if not already built.

        Returns True if the index is ready.
        """
        with self._lock:
            if self._built:
                return True
            if not self._building:
                self._start_build()

        if wait:
            return self._build_event.wait(timeout=timeout)
        return False

    def rebuild(self) -> None:
        """Force a full rebuild."""
        with self._lock:
            self._built = False
            self._build_event.clear()
            self._start_build()

    def refresh(self, file_path: str, content: str | None = None) -> int:
        """Re-index a single file. Returns the new generation."""
        symbols = self._extract_symbols_from_file(file_path, content)
        with self._lock:
            self._generation += 1
            gen = self._generation
            self._symbols[file_path] = _FileSymbols(
                file_path=file_path,
                symbols=symbols,
                generation=gen,
                indexed_at=time.time(),
            )
            # Wake per-file waiters
            evt = self._file_events.get(file_path)
            if evt:
                evt.set()
        return gen

    def wait_for_file(self, file_path: str, timeout: float = 10.0) -> bool:
        """Wait until a specific file has been refreshed."""
        with self._lock:
            if file_path not in self._file_events:
                self._file_events[file_path] = threading.Event()
            evt = self._file_events[file_path]
        return evt.wait(timeout=timeout)

    def find(self, query: str, kind: SymbolKind | None = None) -> list[SymbolInfo]:
        """Find symbols matching a query string."""
        needle = query.lower().strip()
        if not needle:
            return []
        results: list[SymbolInfo] = []
        with self._lock:
            for fs in self._symbols.values():
                for sym in fs.symbols:
                    if needle in sym.name.lower():
                        if kind is None or sym.kind == kind:
                            results.append(sym)
        return results

    def file_symbols(self, file_path: str) -> list[SymbolInfo]:
        """Return all symbols in a specific file."""
        with self._lock:
            fs = self._symbols.get(file_path)
            return list(fs.symbols) if fs else []

    def all_symbols(self) -> list[SymbolInfo]:
        """Return all indexed symbols."""
        with self._lock:
            return [
                sym
                for fs in self._symbols.values()
                for sym in fs.symbols
            ]

    def file_generation(self, file_path: str) -> int:
        """Return the generation counter for a file."""
        with self._lock:
            fs = self._symbols.get(file_path)
            return fs.generation if fs else 0

    def rebuild_prefix(self, prefix: str) -> int:
        """Re-index all files under a path prefix. Returns count refreshed."""
        count = 0
        with self._lock:
            paths = [k for k in self._symbols if k.startswith(prefix)]
        for fp in paths:
            self.refresh(fp)
            count += 1
        return count

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

    # -- Background build -----------------------------------------------------

    def _start_build(self) -> None:
        """Start a background build thread (must hold _lock)."""
        self._building = True
        self._build_thread = threading.Thread(
            target=self._background_build,
            name="symbol-index-build",
            daemon=True,
        )
        self._build_thread.start()

    def _background_build(self) -> None:
        """Index all files in the workspace."""
        try:
            root = Path(self._workspace_root)
            if not root.is_dir():
                logger.warning("Workspace root does not exist: %s", self._workspace_root)
                return

            files = self._collect_files(root)
            logger.info(
                "Symbol index: building for %d files in %s",
                len(files), self._workspace_root,
            )

            batch: list[tuple[str, list[SymbolInfo]]] = []
            for fp in files:
                symbols = self._extract_symbols_from_file(str(fp))
                batch.append((str(fp), symbols))

                if len(batch) >= SYMBOL_INDEX_BATCH_SIZE:
                    self._commit_batch(batch)
                    batch.clear()

            if batch:
                self._commit_batch(batch)

            with self._lock:
                self._built = True
                self._building = False
                self._build_event.set()

            logger.info(
                "Symbol index: built (%d files, %d symbols)",
                self.indexed_files, self.size,
            )

            # Check for pending rebuild
            with self._lock:
                if self._pending_rebuild:
                    self._pending_rebuild = False
                    self._built = False
                    self._build_event.clear()
                    self._start_build()

        except Exception:
            logger.exception("Symbol index build failed")
            with self._lock:
                self._building = False
                self._build_event.set()  # unblock waiters

    def _commit_batch(self, batch: list[tuple[str, list[SymbolInfo]]]) -> None:
        """Commit a batch of indexed files."""
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

    def _collect_files(self, root: Path) -> list[Path]:
        """Collect indexable files under root."""
        files: list[Path] = []
        for path in root.rglob("*"):
            if len(files) >= self._max_files:
                break
            if any(part in SKIP_DIRECTORIES for part in path.parts):
                continue
            if path.is_file() and path.suffix in SUPPORTED_EXTENSIONS:
                files.append(path)
        files.sort()
        return files

    # -- Symbol extraction ----------------------------------------------------

    def _extract_symbols_from_file(
        self, file_path: str, content: str | None = None,
    ) -> list[SymbolInfo]:
        """Extract symbols from a file."""
        if content is None:
            try:
                content = Path(file_path).read_text(encoding="utf-8")
            except Exception:
                return []

        ext = Path(file_path).suffix.lower()
        if ext == ".py":
            return self._extract_python_symbols(file_path, content)
        # For other languages, extract basic patterns
        return self._extract_generic_symbols(file_path, content)

    def _extract_python_symbols(self, file_path: str, content: str) -> list[SymbolInfo]:
        """Extract symbols from Python source using ast."""
        try:
            tree = ast.parse(content, filename=file_path)
        except SyntaxError:
            return []

        symbols: list[SymbolInfo] = []
        self._walk_python_ast(tree, file_path, symbols, container="")
        return symbols

    def _walk_python_ast(
        self,
        node: ast.AST,
        file_path: str,
        bucket: list[SymbolInfo],
        container: str,
    ) -> None:
        """Recursively extract symbols from Python AST."""
        for child in ast.iter_child_nodes(node):
            if isinstance(child, (ast.FunctionDef, ast.AsyncFunctionDef)):
                name = child.name
                full_name = f"{container}.{name}" if container else name
                kind = SymbolKind.METHOD if container else SymbolKind.FUNCTION
                args = [arg.arg for arg in child.args.args]
                signature = f"def {name}({', '.join(args)})"
                bucket.append(SymbolInfo(
                    name=full_name,
                    kind=kind,
                    file_path=file_path,
                    line=child.lineno,
                    character=child.col_offset,
                    signature=signature,
                    docstring=ast.get_docstring(child) or "",
                    container=container,
                ))
                self._walk_python_ast(child, file_path, bucket, full_name)

            elif isinstance(child, ast.ClassDef):
                name = child.name
                full_name = f"{container}.{name}" if container else name
                bucket.append(SymbolInfo(
                    name=full_name,
                    kind=SymbolKind.CLASS,
                    file_path=file_path,
                    line=child.lineno,
                    character=child.col_offset,
                    signature=f"class {name}",
                    docstring=ast.get_docstring(child) or "",
                    container=container,
                ))
                self._walk_python_ast(child, file_path, bucket, full_name)

            elif isinstance(child, ast.Assign):
                for target in child.targets:
                    if isinstance(target, ast.Name):
                        full_name = f"{container}.{target.id}" if container else target.id
                        bucket.append(SymbolInfo(
                            name=full_name,
                            kind=SymbolKind.VARIABLE,
                            file_path=file_path,
                            line=target.lineno,
                            character=target.col_offset,
                            signature=f"{target.id} = ...",
                            container=container,
                        ))
            else:
                self._walk_python_ast(child, file_path, bucket, container)

    def _extract_generic_symbols(self, file_path: str, content: str) -> list[SymbolInfo]:
        """Extract basic symbols from non-Python files using regex patterns."""
        import re
        symbols: list[SymbolInfo] = []
        lines = content.splitlines()

        # Common patterns: function/class/interface/const definitions
        patterns = [
            (r'(?:export\s+)?(?:async\s+)?function\s+(\w+)', SymbolKind.FUNCTION),
            (r'(?:export\s+)?class\s+(\w+)', SymbolKind.CLASS),
            (r'(?:export\s+)?interface\s+(\w+)', SymbolKind.INTERFACE),
            (r'(?:export\s+)?const\s+(\w+)\s*=', SymbolKind.CONSTANT),
            (r'def\s+(\w+)\s*\(', SymbolKind.FUNCTION),
            (r'func\s+(\w+)\s*\(', SymbolKind.FUNCTION),  # Go
            (r'fn\s+(\w+)\s*[<(]', SymbolKind.FUNCTION),  # Rust
        ]

        for lineno, line in enumerate(lines, start=1):
            for pattern, kind in patterns:
                m = re.match(pattern, line.strip())
                if m:
                    symbols.append(SymbolInfo(
                        name=m.group(1),
                        kind=kind,
                        file_path=file_path,
                        line=lineno,
                        character=0,
                        signature=line.strip()[:100],
                    ))
                    break

        return symbols
