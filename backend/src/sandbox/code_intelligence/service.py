"""Per-sandbox :class:`CodeIntelligenceService` facade.

The facade delegates every public op to a :class:`CodeIntelligenceBackend` selected at
construction time. Transport-backed sandbox services use
:class:`DaemonBackend`; sandboxless/local flows keep using
:class:`InProcessBackend`.
"""

from __future__ import annotations

import logging
from collections.abc import Sequence
from typing import Any

from sandbox.api.transport import SandboxTransport
from sandbox.code_intelligence.backends import (
    CodeIntelligenceBackend,
    InProcessBackend,
    DaemonBackend,
)
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

__all__ = ["CodeIntelligenceService"]

logger = logging.getLogger(__name__)


def _select_backend(
    sandbox_id: str,
    workspace_root: str,
    sandbox: Any,
    *,
    transport: SandboxTransport | None,
    edit_history: Any | None = None,
    symbol_index_persistence: Any | None = None,
    daemon_local: bool = False,
) -> CodeIntelligenceBackend:
    """Pick a backend based on transport availability and sandbox identity.

    Transport-backed remote sandboxes use the daemon backend. Local
    sandboxless flows (no transport / empty sandbox_id) keep using
    :class:`InProcessBackend`.

    ``edit_history`` and ``symbol_index_persistence`` are only meaningful for
    the in-process backend (the daemon owns the canonical SQLite ledger and
    SQLite IndexStore when the daemon backend is in use).
    """
    if transport is not None and sandbox_id:
        assert transport is not None  # narrow for type-checker
        return DaemonBackend(
            sandbox_id=sandbox_id,
            workspace_root=workspace_root,
            transport=transport,
        )
    return InProcessBackend(
        sandbox_id=sandbox_id,
        workspace_root=workspace_root,
        sandbox=sandbox,
        transport=transport,
        edit_history=edit_history,
        symbol_index_persistence=symbol_index_persistence,
        daemon_local=daemon_local,
    )


class CodeIntelligenceService:
    """Thin facade that forwards every public op to a selected :class:`CodeIntelligenceBackend`."""

    def __init__(
        self,
        sandbox_id: str,
        workspace_root: str = "/workspace",
        sandbox: Any = None,
        *,
        transport: SandboxTransport | None = None,
        edit_history: Any | None = None,
        symbol_index_persistence: Any | None = None,
        daemon_local: bool = False,
    ) -> None:
        self._impl: CodeIntelligenceBackend = _select_backend(
            sandbox_id,
            workspace_root,
            sandbox,
            transport=transport,
            edit_history=edit_history,
            symbol_index_persistence=symbol_index_persistence,
            daemon_local=daemon_local,
        )

    # -- Identity / state forwarding -----------------------------------------

    @property
    def sandbox_id(self) -> str:
        return self._impl.sandbox_id

    @property
    def workspace_root(self) -> str:
        return self._impl.workspace_root

    @property
    def is_initialized(self) -> bool:
        return self._impl.is_initialized

    # -- Internal-component pass-through (load-bearing for callers) ----------
    # workspace.py, code_intelligence_api.py, and several tests read these
    # attributes directly. They forward to the in-process impl; the daemon
    # backend will surface equivalents in a future phase.

    @property
    def symbol_index(self) -> Any:
        return self._impl.symbol_index  # type: ignore[attr-defined]

    @symbol_index.setter
    def symbol_index(self, value: Any) -> None:
        self._impl.symbol_index = value  # type: ignore[attr-defined]

    @property
    def arbiter(self) -> Any:
        return self._impl.arbiter  # type: ignore[attr-defined]

    @property
    def time_machine(self) -> Any:
        return self._impl.time_machine  # type: ignore[attr-defined]

    @property
    def patcher(self) -> Any:
        return self._impl.patcher  # type: ignore[attr-defined]

    @property
    def lsp_client(self) -> Any:
        return self._impl.lsp_client  # type: ignore[attr-defined]

    @lsp_client.setter
    def lsp_client(self, value: Any) -> None:
        self._impl.lsp_client = value  # type: ignore[attr-defined]

    @property
    def _content(self) -> Any:
        return self._impl._content  # type: ignore[attr-defined]

    @property
    def _write_coordinator(self) -> Any:
        return self._impl._write_coordinator  # type: ignore[attr-defined]

    @property
    def _mutations(self) -> Any:
        return self._impl._mutations  # type: ignore[attr-defined]

    @property
    def _command_executor(self) -> Any:
        return self._impl._command_executor  # type: ignore[attr-defined]

    @property
    def _sandbox(self) -> Any:
        return getattr(self._impl, "_sandbox", None)

    @property
    def _transport(self) -> SandboxTransport | None:
        return getattr(self._impl, "_transport", None)

    # -- Public API forwarding -----------------------------------------------

    def ensure_initialized(self, wait: bool = True) -> bool:
        return self._impl.ensure_initialized(wait=wait)

    def warmup(self) -> None:
        self._impl.warmup()

    def rebind_sandbox(self, sandbox: Any) -> None:
        self._impl.rebind_sandbox(sandbox)

    async def cmd(self, sandbox: Any, command: str, **kwargs: Any) -> Any:
        return await self._impl.cmd(sandbox, command, **kwargs)

    def find_definitions(
        self,
        file_path: str,
        symbol: str,
        line: int = 0,
        character: int = 0,
    ) -> list[SymbolInfo]:
        return self._impl.find_definitions(file_path, symbol, line, character)

    def find_references(
        self,
        file_path: str,
        symbol: str,
        line: int = 0,
        character: int = 0,
    ) -> list[ReferenceInfo]:
        return self._impl.find_references(file_path, symbol, line, character)

    def hover(self, file_path: str, line: int, character: int) -> HoverResult | None:
        return self._impl.hover(file_path, line, character)

    def diagnostics(self, file_path: str) -> list[Diagnostic]:
        return self._impl.diagnostics(file_path)

    def query_symbols(self, query: str) -> list[SymbolInfo]:
        return self._impl.query_symbols(query)

    def apply_edit(self, request: EditRequest) -> EditResult:
        return self._impl.apply_edit(request)

    def commit_operation_against_base(
        self,
        changes: Sequence[OperationChange],
        *,
        agent_id: str = "",
        edit_type: str,
        description: str = "",
    ) -> OperationResult:
        return self._impl.commit_operation_against_base(
            changes,
            agent_id=agent_id,
            edit_type=edit_type,
            description=description,
        )

    def commit_specs_many(
        self,
        requests: Sequence[dict[str, Any]],
    ) -> list[OperationResult]:
        return self._impl.commit_specs_many(requests)

    def list_folder_files(self, folder: str) -> list[str]:
        return self._impl.list_folder_files(folder)

    def write_file(
        self,
        specs: Sequence[WriteSpec] | WriteSpec,
        *,
        agent_id: str = "",
        description: str = "",
    ) -> OperationResult:
        return self._impl.write_file(specs, agent_id=agent_id, description=description)

    def edit_file(
        self,
        specs: Sequence[EditSpec] | EditSpec,
        *,
        agent_id: str = "",
        description: str = "",
    ) -> OperationResult:
        return self._impl.edit_file(specs, agent_id=agent_id, description=description)

    def delete_file(
        self,
        paths: Sequence[str | DeleteSpec],
        *,
        agent_id: str = "",
        description: str = "",
    ) -> OperationResult:
        return self._impl.delete_file(paths, agent_id=agent_id, description=description)

    def move_file(
        self,
        specs: Sequence[MoveSpec],
        *,
        agent_id: str = "",
        description: str = "",
    ) -> OperationResult:
        return self._impl.move_file(specs, agent_id=agent_id, description=description)

    def undo_last_edit(self, file_path: str) -> EditResult:
        return self._impl.undo_last_edit(file_path)

    def status(self) -> dict[str, Any]:
        return self._impl.status()

    def get_telemetry(self) -> CITelemetry:
        return self._impl.get_telemetry()

    def dispose(self) -> None:
        self._impl.dispose()
