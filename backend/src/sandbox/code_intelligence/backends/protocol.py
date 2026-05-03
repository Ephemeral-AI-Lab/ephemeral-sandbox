"""Backend protocol for the CodeIntelligenceService facade."""

from __future__ import annotations

from collections.abc import Sequence
from typing import Any, Protocol

from sandbox.code_intelligence.core.types import (
    CITelemetry,
    DeleteSpec,
    Diagnostic,
    EditRequest,
    EditResult,
    EditSpec,
    HoverResult,
    MoveSpec,
    OperationChange,
    OperationResult,
    ReferenceInfo,
    SymbolInfo,
    WriteSpec,
)


class CodeIntelligenceBackend(Protocol):
    """Shape that every code-intelligence backend implements."""

    sandbox_id: str
    workspace_root: str
    is_initialized: bool

    def ensure_initialized(self, wait: bool = True) -> bool: ...
    def warmup(self) -> None: ...
    def rebind_sandbox(self, sandbox: Any) -> None: ...
    async def cmd(self, sandbox: Any, command: str, **kwargs: Any) -> Any: ...
    def find_definitions(
        self,
        file_path: str,
        symbol: str,
        line: int = 0,
        character: int = 0,
    ) -> list[SymbolInfo]: ...
    def find_references(
        self,
        file_path: str,
        symbol: str,
        line: int = 0,
        character: int = 0,
    ) -> list[ReferenceInfo]: ...
    def hover(self, file_path: str, line: int, character: int) -> HoverResult | None: ...
    def diagnostics(self, file_path: str) -> list[Diagnostic]: ...
    def query_symbols(self, query: str) -> list[SymbolInfo]: ...
    def apply_edit(self, request: EditRequest) -> EditResult: ...
    def commit_operation_against_base(
        self,
        changes: Sequence[OperationChange],
        *,
        agent_id: str = "",
        edit_type: str,
        description: str = "",
    ) -> OperationResult: ...
    def commit_specs_many(
        self,
        requests: Sequence[dict[str, Any]],
    ) -> list[OperationResult]: ...
    def list_folder_files(self, folder: str) -> list[str]: ...
    def write_file(
        self,
        specs: Sequence[WriteSpec] | WriteSpec,
        *,
        agent_id: str = "",
        description: str = "",
    ) -> OperationResult: ...
    def edit_file(
        self,
        specs: Sequence[EditSpec] | EditSpec,
        *,
        agent_id: str = "",
        description: str = "",
    ) -> OperationResult: ...
    def delete_file(
        self,
        paths: Sequence[str | DeleteSpec],
        *,
        agent_id: str = "",
        description: str = "",
    ) -> OperationResult: ...
    def move_file(
        self,
        specs: Sequence[MoveSpec],
        *,
        agent_id: str = "",
        description: str = "",
    ) -> OperationResult: ...
    def undo_last_edit(self, file_path: str) -> EditResult: ...
    def status(self) -> dict[str, Any]: ...
    def get_telemetry(self) -> CITelemetry: ...
    def dispose(self) -> None: ...


__all__ = ["CodeIntelligenceBackend"]
