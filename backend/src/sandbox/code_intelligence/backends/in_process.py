"""In-process CodeIntelligenceService backend."""

from __future__ import annotations

import logging
import threading
from collections.abc import Sequence
from pathlib import Path
from typing import Any

from sandbox.api.transport import SandboxTransport
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
from sandbox.code_intelligence.indexing.symbol_index import SymbolIndex
from sandbox.code_intelligence.language_server.client import LspClient
from sandbox.code_intelligence.mutations.arbiter import Arbiter
from sandbox.code_intelligence.mutations.content_manager import ContentManager
from sandbox.code_intelligence.mutations.mutation_service import MutationService
from sandbox.code_intelligence.mutations.patcher import Patcher
from sandbox.code_intelligence.mutations.time_machine import TimeMachine
from sandbox.code_intelligence.mutations.write_coordinator import WriteCoordinator
from sandbox.code_intelligence.overlay.command_executor import AuditedCommandExecutor
from sandbox.code_intelligence.telemetry import build_status, build_telemetry

__all__ = ["InProcessBackend"]

logger = logging.getLogger(__name__)


class InProcessBackend:
    """In-process backend for local and sandboxless flows."""

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
        self.sandbox_id = sandbox_id
        self.workspace_root = workspace_root
        self._sandbox = sandbox
        self._transport = transport
        self._initialized = False
        self._lsp_bootstrap_attempted = False
        self._init_lock = threading.Lock()

        self.symbol_index = SymbolIndex(
            workspace_root=workspace_root,
            sandbox=sandbox,
            transport=transport,
            sandbox_id=sandbox_id if transport is not None else "",
            persistence=symbol_index_persistence,
        )
        self.arbiter = Arbiter(
            workspace_root=workspace_root,
            edit_history=edit_history,
        )
        self.time_machine = TimeMachine()
        self.patcher = Patcher()
        self.lsp_client = LspClient(
            workspace_root=workspace_root,
            transport=transport,
            sandbox_id=sandbox_id if transport is not None else "",
        )

        self._content = ContentManager(
            workspace_root,
            sandbox=sandbox,
            transport=transport,
            sandbox_id=sandbox_id if transport is not None else "",
        )
        self._write_coordinator = WriteCoordinator(
            arbiter=self.arbiter,
            time_machine=self.time_machine,
            symbol_index=self.symbol_index,
            lsp_client=self.lsp_client,
            content=self._content,
        )
        self._mutations = MutationService(
            content=self._content,
            write_coordinator=self._write_coordinator,
            patcher=self.patcher,
        )
        self._command_executor = AuditedCommandExecutor(
            sandbox_id=sandbox_id,
            workspace_root=workspace_root,
            write_coordinator=self._write_coordinator,
            rebind_sandbox=self.rebind_sandbox,
            transport=transport,
            daemon_local=daemon_local,
        )

    def ensure_initialized(self, wait: bool = True) -> bool:
        with self._init_lock:
            if self._initialized:
                return True

        ready = self.symbol_index.ensure_built(wait=wait)
        lsp_ready = self.lsp_client.ensure_ready(languages=("python",))
        if (
            self._transport is not None
            and self.sandbox_id
            and not lsp_ready.get("python")
            and not self._lsp_bootstrap_attempted
        ):
            self._lsp_bootstrap_attempted = True
            self.lsp_client.ensure_ready(install_missing=True, languages=("python",))

        with self._init_lock:
            self._initialized = ready or self.symbol_index.is_built
        return self.is_initialized

    @property
    def is_initialized(self) -> bool:
        with self._init_lock:
            if self._initialized:
                return True
        if self.symbol_index.is_built:
            with self._init_lock:
                self._initialized = True
            return True
        return False

    def warmup(self) -> None:
        if self.is_initialized:
            return
        try:
            self.ensure_initialized(wait=True)
        except Exception:
            logger.debug("warmup full init failed", exc_info=True)

    def rebind_sandbox(self, sandbox: Any) -> None:
        if sandbox is None:
            return
        self._sandbox = sandbox
        self.symbol_index.bind_sandbox(sandbox)
        self._content.bind_sandbox(sandbox)

    async def cmd(self, sandbox: Any, command: str, **kwargs: Any) -> Any:
        return await self._command_executor.cmd(sandbox, command, **kwargs)

    def find_definitions(
        self,
        file_path: str,
        symbol: str,
        line: int = 0,
        character: int = 0,
    ) -> list[SymbolInfo]:
        if self._is_python(file_path) and line >= 1:
            try:
                results = self.lsp_client.goto_definition(file_path, line, character)
            except Exception as exc:
                logger.warning("LSP definition lookup failed, falling back: %s", exc)
            else:
                if results:
                    return results
        return self.symbol_index.find(symbol)

    def find_references(
        self,
        file_path: str,
        symbol: str,
        line: int = 0,
        character: int = 0,
    ) -> list[ReferenceInfo]:
        del symbol
        if not self._is_python(file_path) or line < 1:
            return []
        try:
            return self.lsp_client.find_references(file_path, line, character)
        except Exception as exc:
            logger.warning("LSP reference lookup failed: %s", exc)
            return []

    def hover(self, file_path: str, line: int, character: int) -> HoverResult | None:
        if self._is_python(file_path) and line >= 1:
            try:
                result = self.lsp_client.hover(file_path, line, character)
            except Exception as exc:
                logger.warning("LSP hover lookup failed, falling back: %s", exc)
            else:
                if result is not None:
                    return result
        for symbol in self.symbol_index.file_symbols(file_path):
            if symbol.line == line:
                return HoverResult(content=symbol.signature or symbol.name, symbol=symbol)
        return None

    def diagnostics(self, file_path: str) -> list[Diagnostic]:
        if not self._is_python(file_path):
            return []
        try:
            return self.lsp_client.diagnostics(file_path)
        except Exception as exc:
            raise RuntimeError(
                f"Diagnostic backend lsp failed and no fallback diagnostic backend succeeded: {exc}"
            ) from exc

    def query_symbols(self, query: str) -> list[SymbolInfo]:
        return self.symbol_index.find(query)

    def apply_edit(self, request: EditRequest) -> EditResult:
        return self._mutations.apply_edit(request)

    def commit_operation_against_base(
        self,
        changes: Sequence[OperationChange],
        *,
        agent_id: str = "",
        edit_type: str,
        description: str = "",
    ) -> OperationResult:
        return self._mutations.commit_operation_against_base(
            changes,
            agent_id=agent_id,
            edit_type=edit_type,
            description=description,
        )

    def commit_specs_many(
        self,
        requests: Sequence[dict[str, Any]],
    ) -> list[OperationResult]:
        return self._mutations.commit_specs_many(requests)

    def list_folder_files(self, folder: str) -> list[str]:
        return self._content.list_folder_files(folder)

    def write_file(
        self,
        specs: Sequence[WriteSpec] | WriteSpec,
        *,
        agent_id: str = "",
        description: str = "",
    ) -> OperationResult:
        return self._mutations.write_file(
            specs,
            agent_id=agent_id,
            description=description,
        )

    def edit_file(
        self,
        specs: Sequence[EditSpec] | EditSpec,
        *,
        agent_id: str = "",
        description: str = "",
    ) -> OperationResult:
        return self._mutations.edit_file(
            specs,
            agent_id=agent_id,
            description=description,
        )

    def delete_file(
        self,
        paths: Sequence[str | DeleteSpec],
        *,
        agent_id: str = "",
        description: str = "",
    ) -> OperationResult:
        return self._mutations.delete_file(
            paths,
            agent_id=agent_id,
            description=description,
        )

    def move_file(
        self,
        specs: Sequence[MoveSpec],
        *,
        agent_id: str = "",
        description: str = "",
    ) -> OperationResult:
        return self._mutations.move_file(
            specs,
            agent_id=agent_id,
            description=description,
        )

    def undo_last_edit(self, file_path: str) -> EditResult:
        return self._mutations.undo_last_edit(file_path)

    def status(self) -> dict[str, Any]:
        return build_status(
            sandbox_id=self.sandbox_id,
            workspace_root=self.workspace_root,
            initialized=self.is_initialized,
            symbol_index=self.symbol_index,
            arbiter=self.arbiter,
            lsp_client=self.lsp_client,
        )

    def get_telemetry(self) -> CITelemetry:
        return build_telemetry(
            symbol_index=self.symbol_index,
            arbiter=self.arbiter,
            lsp_client=self.lsp_client,
        )

    @staticmethod
    def _is_python(file_path: str) -> bool:
        return Path(file_path).suffix.lower() == ".py"

    def dispose(self) -> None:
        self.arbiter.cleanup_locks()
        self.time_machine.clear()
        try:
            self.lsp_client.close()
        except Exception:  # pragma: no cover - defensive
            logger.debug("lsp_client.close() failed during dispose", exc_info=True)
        logger.info("CodeIntelligenceService disposed for sandbox %s", self.sandbox_id)
