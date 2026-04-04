"""Shared data types for the code intelligence service."""

from __future__ import annotations

from dataclasses import dataclass, field
from enum import Enum
from typing import Any


class SymbolKind(str, Enum):
    """Symbol classification."""

    FUNCTION = "function"
    CLASS = "class"
    METHOD = "method"
    VARIABLE = "variable"
    MODULE = "module"
    INTERFACE = "interface"
    PROPERTY = "property"
    CONSTANT = "constant"
    UNKNOWN = "unknown"


class DiagnosticSeverity(str, Enum):
    """LSP-style diagnostic severity."""

    ERROR = "error"
    WARNING = "warning"
    INFORMATION = "information"
    HINT = "hint"


@dataclass(frozen=True)
class SymbolInfo:
    """Resolved symbol location."""

    name: str
    kind: SymbolKind
    file_path: str
    line: int
    character: int = 0
    signature: str = ""
    docstring: str = ""
    container: str = ""


@dataclass(frozen=True)
class ReferenceInfo:
    """A reference to a symbol in a file."""

    file_path: str
    line: int
    character: int = 0
    text: str = ""


@dataclass(frozen=True)
class HoverResult:
    """Hover information for a position."""

    content: str
    language: str = ""
    symbol: SymbolInfo | None = None


@dataclass(frozen=True)
class Diagnostic:
    """A single diagnostic (error, warning, etc.)."""

    file_path: str
    line: int
    character: int = 0
    end_line: int | None = None
    end_character: int | None = None
    severity: DiagnosticSeverity = DiagnosticSeverity.ERROR
    message: str = ""
    source: str = ""
    code: str = ""


@dataclass(frozen=True)
class EditRequest:
    """A request to edit a file via OCC."""

    file_path: str
    old_text: str
    new_text: str
    agent_id: str = ""
    description: str = ""


@dataclass(frozen=True)
class EditResult:
    """Result of an edit operation."""

    success: bool
    file_path: str
    message: str = ""
    conflict: bool = False
    snapshot_id: str = ""


@dataclass
class CITelemetry:
    """Runtime telemetry for the code intelligence service."""

    tree_cache_size: int = 0
    tree_cache_hits: int = 0
    tree_cache_misses: int = 0
    symbol_index_size: int = 0
    symbol_index_generation: int = 0
    indexed_files: int = 0
    lsp_connected: bool = False
    lsp_query_count: int = 0
    lsp_cache_hits: int = 0
    arbiter_active_edits: int = 0
    ledger_entry_count: int = 0
    extra: dict[str, Any] = field(default_factory=dict)


@dataclass(frozen=True)
class FileSnapshot:
    """A point-in-time snapshot of a file for undo."""

    file_path: str
    content: str
    snapshot_id: str
    timestamp: float = 0.0


@dataclass(frozen=True)
class LedgerEntry:
    """An entry in the edit audit journal."""

    file_path: str
    agent_id: str
    edit_type: str  # "edit", "create", "delete", "shell_mutation"
    timestamp: float = 0.0
    description: str = ""
    old_hash: str = ""
    new_hash: str = ""
