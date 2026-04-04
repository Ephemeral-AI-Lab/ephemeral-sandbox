"""Backend protocol — uniform interface for CI query backends.

Defines the ``CodeIntelligenceBackend`` protocol so the query router
can dispatch queries without knowing backend internals. Includes
adapters for LSP and SymbolIndex backends.
"""

from __future__ import annotations

from dataclasses import dataclass
from enum import Enum
from typing import Any, Protocol, runtime_checkable

from ephemeralos.services.code_intelligence.types import (
    Diagnostic,
    HoverResult,
    ReferenceInfo,
    SymbolInfo,
)


class QueryStatus(str, Enum):
    """Outcome of a backend query."""

    SUCCESS = "success"
    EMPTY = "empty"
    UNSUPPORTED = "unsupported"
    UNAVAILABLE = "unavailable"
    ERROR = "error"


@dataclass(frozen=True)
class BackendQueryOutcome:
    """Result wrapper from a backend query."""

    status: QueryStatus
    results: list[Any] | None = None
    error: str = ""


@runtime_checkable
class CodeIntelligenceBackend(Protocol):
    """Protocol interface for CI query backends."""

    @property
    def name(self) -> str: ...

    @property
    def priority(self) -> int: ...

    def supports(self, file_path: str) -> bool: ...

    def find_definitions(
        self, file_path: str, symbol: str, line: int, character: int,
    ) -> BackendQueryOutcome: ...

    def find_references(
        self, file_path: str, symbol: str, line: int, character: int,
    ) -> BackendQueryOutcome: ...

    def hover(
        self, file_path: str, line: int, character: int,
    ) -> BackendQueryOutcome: ...

    def diagnostics(self, file_path: str) -> BackendQueryOutcome: ...


class SymbolIndexBackendAdapter:
    """Adapts SymbolIndex to the CodeIntelligenceBackend protocol.

    Priority: 50 (structural fallback).
    """

    def __init__(self, symbol_index: Any) -> None:
        self._index = symbol_index

    @property
    def name(self) -> str:
        return "symbol_index"

    @property
    def priority(self) -> int:
        return 50

    def supports(self, file_path: str) -> bool:
        return True  # Can attempt all file types

    def find_definitions(
        self, file_path: str, symbol: str, line: int, character: int,
    ) -> BackendQueryOutcome:
        results = self._index.find(symbol)
        if not results:
            return BackendQueryOutcome(status=QueryStatus.EMPTY)
        return BackendQueryOutcome(
            status=QueryStatus.SUCCESS,
            results=results,
        )

    def find_references(
        self, file_path: str, symbol: str, line: int, character: int,
    ) -> BackendQueryOutcome:
        # SymbolIndex doesn't support reference search (semantic-only)
        return BackendQueryOutcome(status=QueryStatus.UNSUPPORTED)

    def hover(
        self, file_path: str, line: int, character: int,
    ) -> BackendQueryOutcome:
        # Try to find the symbol at the given position
        symbols = self._index.file_symbols(file_path)
        for sym in symbols:
            if sym.line == line:
                hover = HoverResult(
                    content=sym.signature or sym.name,
                    symbol=sym,
                )
                return BackendQueryOutcome(
                    status=QueryStatus.SUCCESS,
                    results=[hover],
                )
        return BackendQueryOutcome(status=QueryStatus.EMPTY)

    def diagnostics(self, file_path: str) -> BackendQueryOutcome:
        # SymbolIndex can report parse errors if the file failed to index
        return BackendQueryOutcome(status=QueryStatus.UNSUPPORTED)


class LspBackendAdapter:
    """Adapts LspClient to the CodeIntelligenceBackend protocol.

    Priority: 100 (semantic queries preferred).
    """

    _SUPPORTED_EXTENSIONS = {".py", ".js", ".ts", ".jsx", ".tsx"}

    def __init__(self, lsp_client: Any) -> None:
        self._lsp = lsp_client

    @property
    def name(self) -> str:
        return "lsp"

    @property
    def priority(self) -> int:
        return 100

    def supports(self, file_path: str) -> bool:
        from pathlib import Path
        return Path(file_path).suffix.lower() in self._SUPPORTED_EXTENSIONS

    def find_definitions(
        self, file_path: str, symbol: str, line: int, character: int,
    ) -> BackendQueryOutcome:
        try:
            results = self._lsp.goto_definition(file_path, line, character)
            if not results:
                return BackendQueryOutcome(status=QueryStatus.EMPTY)
            return BackendQueryOutcome(status=QueryStatus.SUCCESS, results=results)
        except Exception as e:
            return BackendQueryOutcome(status=QueryStatus.ERROR, error=str(e))

    def find_references(
        self, file_path: str, symbol: str, line: int, character: int,
    ) -> BackendQueryOutcome:
        try:
            results = self._lsp.find_references(file_path, line, character)
            if not results:
                return BackendQueryOutcome(status=QueryStatus.EMPTY)
            return BackendQueryOutcome(status=QueryStatus.SUCCESS, results=results)
        except Exception as e:
            return BackendQueryOutcome(status=QueryStatus.ERROR, error=str(e))

    def hover(
        self, file_path: str, line: int, character: int,
    ) -> BackendQueryOutcome:
        try:
            result = self._lsp.hover(file_path, line, character)
            if result is None:
                return BackendQueryOutcome(status=QueryStatus.EMPTY)
            return BackendQueryOutcome(status=QueryStatus.SUCCESS, results=[result])
        except Exception as e:
            return BackendQueryOutcome(status=QueryStatus.ERROR, error=str(e))

    def diagnostics(self, file_path: str) -> BackendQueryOutcome:
        try:
            results = self._lsp.diagnostics(file_path)
            if not results:
                return BackendQueryOutcome(status=QueryStatus.EMPTY)
            return BackendQueryOutcome(status=QueryStatus.SUCCESS, results=results)
        except Exception as e:
            return BackendQueryOutcome(status=QueryStatus.ERROR, error=str(e))
