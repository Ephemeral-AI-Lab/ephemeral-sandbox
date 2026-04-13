"""Background symbol indexing for a workspace."""

from __future__ import annotations

import asyncio
import ast
import concurrent.futures
import inspect
import logging
import posixpath
import re
import threading
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from code_intelligence.constants import (
    SKIP_DIRECTORIES,
    SUPPORTED_EXTENSIONS,
    SYMBOL_INDEX_BATCH_SIZE,
    SYMBOL_INDEX_MAX_FILES,
)
from code_intelligence.types import SymbolInfo, SymbolKind

logger = logging.getLogger(__name__)

_GENERIC_SYMBOL_PATTERNS: tuple[tuple[re.Pattern[str], SymbolKind], ...] = (
    (re.compile(r"(?:export\s+)?(?:async\s+)?function\s+(\w+)"), SymbolKind.FUNCTION),
    (re.compile(r"(?:export\s+)?class\s+(\w+)"), SymbolKind.CLASS),
    (re.compile(r"(?:export\s+)?interface\s+(\w+)"), SymbolKind.INTERFACE),
    (re.compile(r"(?:export\s+)?const\s+(\w+)\s*="), SymbolKind.CONSTANT),
    (re.compile(r"def\s+(\w+)\s*\("), SymbolKind.FUNCTION),
    (re.compile(r"func\s+(\w+)\s*\("), SymbolKind.FUNCTION),
    (re.compile(r"fn\s+(\w+)\s*[<(]"), SymbolKind.FUNCTION),
)


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
        tree_cache: Any | None = None,
    ) -> None:
        self._workspace_root = workspace_root
        self._max_files = max_files
        self._sandbox = sandbox
        self._tree_cache = tree_cache

        self._lock = threading.Lock()
        self._symbols: dict[str, _FileSymbols] = {}
        self._built = False
        self._building = False
        self._build_event = threading.Event()
        self._generation = 0
        self._file_events: dict[str, threading.Event] = {}
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
            self._build_event.wait(timeout=timeout)
            with self._lock:
                return self._built
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
            evt = self._file_events.get(file_path)
            if evt:
                evt.set()
        return gen

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

    def symbol_boundaries_for_file(self, file_path: str) -> list[tuple[str, int, int]]:
        """Return ``(symbol_name, start_line, end_line)`` for indexed symbols in *file_path*."""
        with self._lock:
            fs = self._symbols.get(file_path)
            if fs is None:
                return []
            return [
                (symbol.name, symbol.line, symbol.end_line or symbol.line)
                for symbol in fs.symbols
                if symbol.line > 0
            ]

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
        """Start a background build thread (must hold _lock)."""
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
            root = Path(self._workspace_root)
            if root.is_dir():
                files = [str(fp) for fp in self._collect_local_files(root)]
            else:
                files = self._collect_remote_files(self._workspace_root)
                if files is None:
                    logger.warning("Workspace root does not exist: %s", self._workspace_root)
                    with self._lock:
                        self._building = False
                        self._build_event.set()
                    return

            logger.info(
                "Symbol index: building for %d files in %s",
                len(files), self._workspace_root,
            )

            # For remote sandboxes, download files concurrently to reduce
            # HTTP round-trips (the main cold-start bottleneck).
            if self._sandbox is not None:
                self._build_remote_parallel(files)
            else:
                self._build_sequential(files)

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

    def _build_sequential(self, files: list[str | Path]) -> None:
        """Index files one at a time (local filesystem)."""
        batch: list[tuple[str, list[SymbolInfo]]] = []
        for file_path in files:
            symbols = self._extract_symbols_from_file(str(file_path))
            batch.append((str(file_path), symbols))
            if len(batch) >= SYMBOL_INDEX_BATCH_SIZE:
                self._commit_batch(batch)
                batch.clear()
        if batch:
            self._commit_batch(batch)

    def _build_remote_parallel(self, files: list[str]) -> None:
        """Download remote files via the batch API and index them.

        Uses ``sandbox.fs.download_files`` to fetch all files in a single
        multipart HTTP stream, eliminating per-file connection overhead
        that made cold starts take ~20 min for 300 files.
        """
        sandbox = self._sandbox
        fs = getattr(sandbox, "fs", None) if sandbox is not None else None
        download_files_fn = getattr(fs, "download_files", None)

        # Only use batch API if the sandbox exposes a real download_files
        # (not an auto-generated MagicMock attribute).
        has_batch = False
        if callable(download_files_fn):
            try:
                from daytona_sdk.common.filesystem import FileDownloadRequest  # noqa: F401

                # Verify it's from the real SDK, not a mock auto-attr
                has_batch = hasattr(download_files_fn, "__func__") or hasattr(
                    download_files_fn, "__wrapped__"
                )
                if not has_batch:
                    # Also accept if the module path looks like daytona_sdk
                    mod = getattr(type(fs), "__module__", "") or ""
                    has_batch = "daytona" in mod
            except ImportError:
                pass

        if not has_batch:
            self._build_remote_individual(files)
            return

        from daytona_sdk.common.filesystem import FileDownloadRequest

        _BATCH_DOWNLOAD_SIZE = 50

        for i in range(0, len(files), _BATCH_DOWNLOAD_SIZE):
            chunk = files[i : i + _BATCH_DOWNLOAD_SIZE]
            try:
                requests = [FileDownloadRequest(source=fp) for fp in chunk]
                responses = self._resolve(download_files_fn(requests))
            except Exception:
                logger.debug(
                    "Batch download_files failed for chunk %d–%d, falling back",
                    i, i + len(chunk), exc_info=True,
                )
                self._build_remote_individual(chunk)
                continue

            commit_batch: list[tuple[str, list[SymbolInfo]]] = []
            for resp in responses or []:
                fp = getattr(resp, "source", None)
                if fp is None:
                    continue
                if getattr(resp, "error", None) or getattr(resp, "result", None) is None:
                    logger.debug("Batch download skipped %s: %s", fp, getattr(resp, "error", "no data"))
                    continue
                raw = resp.result
                content = raw.decode("utf-8") if isinstance(raw, bytes) else str(raw)
                symbols = self._extract_symbols_from_file(fp, content)
                commit_batch.append((fp, symbols))

            if commit_batch:
                self._commit_batch(commit_batch)

    def _build_remote_individual(self, files: list[str]) -> None:
        """Fallback: download files individually using a thread pool."""
        _POOL_SIZE = 8

        def _download_and_extract(fp: str) -> tuple[str, list[SymbolInfo]]:
            content = self._read_file_content(fp)
            symbols = self._extract_symbols_from_file(fp, content) if content else []
            return (fp, symbols)

        with concurrent.futures.ThreadPoolExecutor(max_workers=_POOL_SIZE) as pool:
            batch: list[tuple[str, list[SymbolInfo]]] = []
            for result in pool.map(_download_and_extract, files):
                batch.append(result)
                if len(batch) >= SYMBOL_INDEX_BATCH_SIZE:
                    self._commit_batch(batch)
                    batch.clear()
            if batch:
                self._commit_batch(batch)

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

    def _collect_local_files(self, root: Path) -> list[Path]:
        """Collect indexable files from a local workspace root."""
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

    def _collect_remote_files(self, root: str) -> list[str] | None:
        """Collect indexable files from a sandbox workspace root.

        Prefers ``sandbox.fs.search_files`` (single glob API call) when
        available, falling back to recursive ``list_files`` traversal.
        """
        sandbox = self._sandbox
        fs = getattr(sandbox, "fs", None) if sandbox is not None else None

        # Try fast path: search_files with glob patterns (1 HTTP call per ext)
        files = self._collect_remote_files_via_search(fs, root)
        if files is not None:
            return files

        # Fallback: recursive directory traversal
        return self._collect_remote_files_via_list(fs, root)

    def _collect_remote_files_via_search(
        self, fs: Any, root: str,
    ) -> list[str] | None:
        """Use sandbox.fs.search_files to discover files in one call."""
        search_fn = getattr(fs, "search_files", None)
        if not callable(search_fn):
            return None
        # Verify it's a real SDK method, not a MagicMock auto-attr
        mod = getattr(type(fs), "__module__", "") or ""
        if "daytona" not in mod:
            return None

        try:
            result = self._resolve(search_fn(root, "*.{py,js,ts,jsx,tsx,java,go,rs,rb,php,c,cpp,h,hpp,cs,swift,kt,scala,sh}"))
            raw_files = getattr(result, "files", None) or []
        except Exception:
            logger.debug("search_files failed, falling back to list_files", exc_info=True)
            return None

        files: list[str] = []
        for fp in raw_files:
            if len(files) >= self._max_files:
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

    def _collect_remote_files_via_list(
        self, fs: Any, root: str,
    ) -> list[str] | None:
        """Fallback: recursive list_files directory traversal."""
        list_files_fn = getattr(fs, "list_files", None)
        if not callable(list_files_fn):
            return None

        files: list[str] = []
        pending: list[str] = [str(root).rstrip("/") or "/"]

        while pending and len(files) < self._max_files:
            current = pending.pop()
            try:
                entries = self._resolve(list_files_fn(current)) or []
            except Exception:
                logger.debug("Remote list_files failed for %s", current, exc_info=True)
                return None

            for entry in entries:
                if len(files) >= self._max_files:
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

    # -- Symbol extraction ----------------------------------------------------

    def _extract_symbols_from_file(
        self, file_path: str, content: str | None = None,
    ) -> list[SymbolInfo]:
        """Extract symbols from a file."""
        if content is None:
            content = self._read_file_content(file_path)
            if content is None:
                return []

        ext = Path(file_path).suffix.lower()
        if ext == ".py":
            return self._extract_python_symbols(file_path, content)
        # Try tree-sitter for richer non-Python symbols
        tree_sitter_symbols = self._extract_tree_sitter_symbols(file_path, content)
        if tree_sitter_symbols:
            return tree_sitter_symbols
        # Fallback to regex patterns
        return self._extract_generic_symbols(file_path, content)

    def _extract_tree_sitter_symbols(self, file_path: str, content: str) -> list[SymbolInfo]:
        """Extract non-Python symbols from a cached tree-sitter parse when available."""
        if self._tree_cache is None:
            return []
        entry = self._tree_cache.get_tree(file_path, content=content)
        if entry is None:
            return []
        root = getattr(entry.tree, "root_node", None)
        if root is None:
            return []

        symbols: list[SymbolInfo] = []
        self._walk_tree_sitter(root, file_path, content, symbols, container="")
        return symbols

    def _walk_tree_sitter(
        self,
        node: Any,
        file_path: str,
        content: str,
        bucket: list[SymbolInfo],
        container: str,
    ) -> None:
        node_type = str(getattr(node, "type", "") or "")
        current_container = container

        if node_type in {"class_declaration", "class_definition"}:
            symbol = self._tree_sitter_symbol(node, file_path, content, SymbolKind.CLASS, container)
            if symbol is not None:
                bucket.append(symbol)
                current_container = symbol.name
        elif node_type in {"function_declaration", "generator_function_declaration"}:
            symbol = self._tree_sitter_symbol(node, file_path, content, SymbolKind.FUNCTION, container)
            if symbol is not None:
                bucket.append(symbol)
        elif node_type in {"method_definition", "method_signature"}:
            symbol = self._tree_sitter_symbol(node, file_path, content, SymbolKind.METHOD, container)
            if symbol is not None:
                bucket.append(symbol)
        elif node_type == "interface_declaration":
            symbol = self._tree_sitter_symbol(node, file_path, content, SymbolKind.INTERFACE, container)
            if symbol is not None:
                bucket.append(symbol)
                current_container = symbol.name
        elif node_type == "variable_declarator":
            kind = (
                SymbolKind.CONSTANT
                if self._is_const_declaration(node, content)
                else SymbolKind.VARIABLE
            )
            symbol = self._tree_sitter_symbol(node, file_path, content, kind, container)
            if symbol is not None:
                bucket.append(symbol)
        elif node_type in {"public_field_definition", "field_definition"}:
            symbol = self._tree_sitter_symbol(node, file_path, content, SymbolKind.PROPERTY, container)
            if symbol is not None:
                bucket.append(symbol)

        for child in self._node_children(node):
            self._walk_tree_sitter(child, file_path, content, bucket, current_container)

    def _tree_sitter_symbol(
        self,
        node: Any,
        file_path: str,
        content: str,
        kind: SymbolKind,
        container: str,
    ) -> SymbolInfo | None:
        name_node = self._node_name(node)
        if name_node is None:
            return None
        name = self._node_text(name_node, content).strip()
        if not name:
            return None
        full_name = f"{container}.{name}" if container and kind in {SymbolKind.METHOD, SymbolKind.PROPERTY} else name
        start_line, start_char = self._node_start(node)
        end_line, _ = self._node_end(node)
        signature = self._signature_text(node, content)
        return SymbolInfo(
            name=full_name,
            kind=kind,
            file_path=file_path,
            line=start_line,
            end_line=end_line,
            character=start_char,
            signature=signature,
            container=container,
        )

    @staticmethod
    def _node_children(node: Any) -> list[Any]:
        children = getattr(node, "children", None)
        if children is None:
            return []
        return list(children)

    def _node_name(self, node: Any) -> Any | None:
        child_by_field = getattr(node, "child_by_field_name", None)
        if callable(child_by_field):
            named = child_by_field("name")
            if named is not None:
                return named
        for child in self._node_children(node):
            child_type = str(getattr(child, "type", "") or "")
            if child_type in {
                "identifier",
                "type_identifier",
                "property_identifier",
                "shorthand_property_identifier",
            }:
                return child
        return None

    @staticmethod
    def _node_text(node: Any, content: str) -> str:
        start_byte = getattr(node, "start_byte", None)
        end_byte = getattr(node, "end_byte", None)
        if isinstance(start_byte, int) and isinstance(end_byte, int):
            return content[start_byte:end_byte]
        return str(getattr(node, "text", "") or "")

    @staticmethod
    def _point_to_position(point: Any) -> tuple[int, int]:
        if isinstance(point, tuple) and len(point) >= 2:
            return int(point[0]) + 1, int(point[1])
        row = getattr(point, "row", None)
        column = getattr(point, "column", None)
        if row is not None and column is not None:
            return int(row) + 1, int(column)
        return 0, 0

    def _node_start(self, node: Any) -> tuple[int, int]:
        return self._point_to_position(getattr(node, "start_point", None))

    def _node_end(self, node: Any) -> tuple[int, int]:
        return self._point_to_position(getattr(node, "end_point", None))

    def _signature_text(self, node: Any, content: str) -> str:
        text = self._node_text(node, content).strip()
        if not text:
            return ""
        return text.splitlines()[0][:100]

    def _is_const_declaration(self, node: Any, content: str) -> bool:
        current = getattr(node, "parent", None)
        while current is not None:
            current_type = str(getattr(current, "type", "") or "")
            if current_type == "lexical_declaration":
                for child in self._node_children(current):
                    child_type = str(getattr(child, "type", "") or "")
                    if child_type == "const":
                        return True
                    child_text = self._node_text(child, content).strip()
                    if child_text == "const":
                        return True
                return False
            current = getattr(current, "parent", None)
        return False

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
                bucket.append(
                    self._build_symbol_info(
                        file_path=file_path,
                        node=child,
                        name=full_name,
                        kind=kind,
                        signature=f"def {name}({', '.join(args)})",
                        docstring=ast.get_docstring(child) or "",
                        container=container,
                    )
                )
                self._walk_python_ast(child, file_path, bucket, full_name)

            elif isinstance(child, ast.ClassDef):
                name = child.name
                full_name = f"{container}.{name}" if container else name
                bucket.append(
                    self._build_symbol_info(
                        file_path=file_path,
                        node=child,
                        name=full_name,
                        kind=SymbolKind.CLASS,
                        signature=f"class {name}",
                        docstring=ast.get_docstring(child) or "",
                        container=container,
                    )
                )
                self._walk_python_ast(child, file_path, bucket, full_name)

            elif isinstance(child, ast.Assign):
                for target in child.targets:
                    if isinstance(target, ast.Name):
                        full_name = f"{container}.{target.id}" if container else target.id
                        bucket.append(
                            self._build_symbol_info(
                                file_path=file_path,
                                node=target,
                                name=full_name,
                                kind=SymbolKind.VARIABLE,
                                signature=f"{target.id} = ...",
                                container=container,
                            )
                        )
            else:
                self._walk_python_ast(child, file_path, bucket, container)

    def _extract_generic_symbols(self, file_path: str, content: str) -> list[SymbolInfo]:
        """Extract basic symbols from non-Python files using regex patterns."""
        symbols: list[SymbolInfo] = []
        lines = content.splitlines()

        for lineno, line in enumerate(lines, start=1):
            stripped = line.strip()
            for pattern, kind in _GENERIC_SYMBOL_PATTERNS:
                m = pattern.match(stripped)
                if m:
                    symbols.append(
                        SymbolInfo(
                            name=m.group(1),
                            kind=kind,
                            file_path=file_path,
                            line=lineno,
                            end_line=lineno,
                            character=0,
                            signature=stripped[:100],
                        )
                    )
                    break

        return symbols

    def _read_file_content(self, file_path: str) -> str | None:
        try:
            return Path(file_path).read_text(encoding="utf-8")
        except Exception:
            pass

        sandbox = self._sandbox
        fs = getattr(sandbox, "fs", None) if sandbox is not None else None
        download_fn = getattr(fs, "download_file", None)
        if not callable(download_fn):
            return None

        try:
            raw = self._resolve(download_fn(file_path))
        except Exception:
            logger.debug("Remote download_file failed for %s", file_path, exc_info=True)
            return None

        if isinstance(raw, bytes):
            return raw.decode("utf-8")
        return str(raw)

    @staticmethod
    def _resolve(result: Any) -> Any:
        """Resolve possibly-async sandbox results inside sync indexing code."""
        if inspect.isawaitable(result):
            try:
                loop = asyncio.get_running_loop()
            except RuntimeError:
                loop = None
            if loop and loop.is_running():
                with concurrent.futures.ThreadPoolExecutor(max_workers=1) as pool:
                    return pool.submit(asyncio.run, result).result()
            return asyncio.run(result)
        return result

    @staticmethod
    def _build_symbol_info(
        *,
        file_path: str,
        node: ast.AST,
        name: str,
        kind: SymbolKind,
        signature: str,
        docstring: str = "",
        container: str = "",
    ) -> SymbolInfo:
        return SymbolInfo(
            name=name,
            kind=kind,
            file_path=file_path,
            line=getattr(node, "lineno", 0),
            end_line=getattr(node, "end_lineno", getattr(node, "lineno", 0)),
            character=getattr(node, "col_offset", 0),
            signature=signature,
            docstring=docstring,
            container=container,
        )
