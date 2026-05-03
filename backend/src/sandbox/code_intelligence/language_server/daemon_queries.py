"""Daemon-backed language-server query methods."""

from __future__ import annotations

from typing import Any, Protocol

from sandbox.code_intelligence.daemon.wire import (
    diagnostic_from_dict,
    hover_result_from_dict,
    reference_info_from_dict,
    symbol_info_from_dict,
)
from sandbox.code_intelligence.core.types import (
    Diagnostic,
    HoverResult,
    ReferenceInfo,
    SymbolInfo,
)


class _DaemonCommandCaller(Protocol):
    def _call_sync(self, op: str, args: dict[str, Any] | None = None) -> Any: ...


class DaemonLanguageServerQueries:
    """Language-server query methods for a daemon command caller."""

    def find_definitions(
        self: _DaemonCommandCaller,
        file_path: str,
        symbol: str,
        line: int = 0,
        character: int = 0,
    ) -> list[SymbolInfo]:
        rows = self._call_sync(
            "find_definitions",
            {
                "file_path": file_path,
                "symbol": symbol,
                "line": line,
                "character": character,
            },
        )
        return [symbol_info_from_dict(r) for r in (rows or [])]

    def find_references(
        self: _DaemonCommandCaller,
        file_path: str,
        symbol: str,
        line: int = 0,
        character: int = 0,
    ) -> list[ReferenceInfo]:
        rows = self._call_sync(
            "find_references",
            {
                "file_path": file_path,
                "symbol": symbol,
                "line": line,
                "character": character,
            },
        )
        return [reference_info_from_dict(r) for r in (rows or [])]

    def hover(
        self: _DaemonCommandCaller,
        file_path: str,
        line: int,
        character: int,
    ) -> HoverResult | None:
        result = self._call_sync(
            "hover",
            {"file_path": file_path, "line": line, "character": character},
        )
        return hover_result_from_dict(result) if result else None

    def diagnostics(
        self: _DaemonCommandCaller,
        file_path: str,
    ) -> list[Diagnostic]:
        rows = self._call_sync("diagnostics", {"file_path": file_path})
        return [diagnostic_from_dict(r) for r in (rows or [])]
