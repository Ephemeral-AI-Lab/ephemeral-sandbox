"""Background symbol indexing for a workspace."""

from __future__ import annotations

import ast
import logging
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
        tree_cache: Any | None = None,
    ) -> None:
        self._workspace_root = workspace_root
        self._max_files = max_files
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
            if not root.is_dir():
                logger.warning("Workspace root does not exist: %s", self._workspace_root)
                with self._lock:
                    self._building = False
                    self._build_event.set()
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
            content = self._read_file_content(file_path)
            if content is None:
                return []

        ext = Path(file_path).suffix.lower()
        if ext == ".py":
            return self._extract_python_symbols(file_path, content)
        tree_sitter_symbols = self._extract_tree_sitter_symbols(file_path, content)
        if tree_sitter_symbols:
            return tree_sitter_symbols
        # For other languages, extract basic patterns.
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

    @staticmethod
    def _read_file_content(file_path: str) -> str | None:
        try:
            return Path(file_path).read_text(encoding="utf-8")
        except Exception:
            return None

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
