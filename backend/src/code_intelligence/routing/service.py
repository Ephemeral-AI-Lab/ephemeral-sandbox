"""Per-sandbox :class:`CodeIntelligenceService` facade.

This module wires together the analysis, editing, command-audit, and routing
components. Heavy concerns live in focused modules and are delegated from the
service so existing callers can keep importing from ``routing.service``.
"""

from __future__ import annotations

import logging
import threading
from typing import Any

from code_intelligence.analysis.symbol_index import SymbolIndex
from code_intelligence.analysis.tree_cache import TreeCache
from code_intelligence.editing.arbiter import Arbiter
from code_intelligence.editing.patcher import Patcher
from code_intelligence.editing.time_machine import TimeMachine
from code_intelligence.editing.write_coordinator import WriteCoordinator
from code_intelligence.lsp.client import LspClient
from code_intelligence.routing.backend_protocol import (
    LspBackendAdapter,
    SymbolIndexBackendAdapter,
)
from code_intelligence.routing.command_executor import AuditedCommandExecutor
from code_intelligence.routing.content_manager import ContentManager
from code_intelligence.routing.mutation_service import MutationService
from code_intelligence.routing.query_router import IntelligenceQueryRouter
from code_intelligence.routing.registry import (
    dispose_all_code_intelligence,
    dispose_code_intelligence,
    get_all_services_status,
    get_code_intelligence,
    get_code_intelligence_if_exists,
)
from code_intelligence.routing.rename_planner import RenamePlanner
from code_intelligence.routing.telemetry import build_status, build_telemetry

__all__ = [
    "CodeIntelligenceService",
    "dispose_all_code_intelligence",
    "dispose_code_intelligence",
    "get_all_services_status",
    "get_code_intelligence",
    "get_code_intelligence_if_exists",
]

logger = logging.getLogger(__name__)


class CodeIntelligenceService:
    """Thin orchestrator for code intelligence queries and edits in one sandbox."""

    def __init__(
        self,
        sandbox_id: str,
        workspace_root: str = "/workspace",
        sandbox: Any = None,
    ) -> None:
        self.sandbox_id = sandbox_id
        self.workspace_root = workspace_root
        self._sandbox = sandbox
        self._initialized = False
        self._lsp_bootstrap_attempted = False
        self._init_lock = threading.Lock()

        self.tree_cache = TreeCache(sandbox=sandbox)
        self.symbol_index = SymbolIndex(
            workspace_root=workspace_root,
            sandbox=sandbox,
            tree_cache=self.tree_cache,
        )
        self.arbiter = Arbiter(workspace_root=workspace_root)
        self.time_machine = TimeMachine()
        self.patcher = Patcher()
        self.lsp_client = LspClient(workspace_root=workspace_root, sandbox=sandbox)
        self.query_router = IntelligenceQueryRouter()
        self.query_router.register(LspBackendAdapter(self.lsp_client))
        self.query_router.register(SymbolIndexBackendAdapter(self.symbol_index))

        self._content = ContentManager(workspace_root, sandbox=sandbox)
        self._write_coordinator = WriteCoordinator(
            arbiter=self.arbiter,
            time_machine=self.time_machine,
            patcher=self.patcher,
            symbol_index=self.symbol_index,
            lsp_client=self.lsp_client,
            content=self._content,
        )
        self._rename_planner = RenamePlanner(
            workspace_root=workspace_root,
            sandbox=sandbox,
            content=self._content,
            lsp_client=self.lsp_client,
            arbiter=self.arbiter,
            symbol_index=self.symbol_index,
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
        )

    def ensure_initialized(self, wait: bool = True) -> bool:
        """Initialize symbol indexing and LSP. Returns True once ready."""
        with self._init_lock:
            if self._initialized:
                return True

        ready = self.symbol_index.ensure_built(wait=wait)
        lsp_ready = self.lsp_client.ensure_ready(languages=("python",))
        if (
            self._sandbox is not None
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

    def rebind_sandbox(self, sandbox: Any) -> None:
        """Refresh the sandbox handle on this service and its collaborators."""
        if sandbox is None:
            return
        self._sandbox = sandbox
        self.symbol_index.bind_sandbox(sandbox)
        old_sandbox = getattr(self.lsp_client, "_sandbox", None)
        self.lsp_client._sandbox = sandbox
        if old_sandbox is not sandbox:
            self.lsp_client.reset_backend_availability()
            self._rename_planner.clear_cache()
        self._content.bind_sandbox(sandbox)
        self._rename_planner.bind_sandbox(sandbox)

    async def cmd(self, sandbox: Any, command: str, **kwargs: Any) -> Any:
        return await self._command_executor.cmd(sandbox, command, **kwargs)

    async def warmup_overlay(self, sandbox: Any) -> None:
        """Pre-upload the overlay runner script to avoid first-burst stall."""
        await self._command_executor.warmup(sandbox)

    def find_definitions(self, file_path: str, symbol: str, line: int = 0, character: int = 0):
        return self.query_router.find_definitions(file_path, symbol, line, character)

    def find_references(self, file_path: str, symbol: str, line: int = 0, character: int = 0):
        return self.query_router.find_references(file_path, symbol, line, character)

    def hover(self, file_path: str, line: int, character: int):
        return self.query_router.hover(file_path, line, character)

    def diagnostics(self, file_path: str):
        return self.query_router.diagnostics(file_path)

    def query_symbols(self, query: str):
        return self.symbol_index.find(query)

    def rename_symbol_plan(self, file_path: str, line: int, character: int, new_name: str):
        return self._rename_planner.rename_symbol_plan(file_path, line, character, new_name)

    def rename_symbol_plans_many(self, requests: Any):
        return self._rename_planner.rename_symbol_plans_many(requests)

    def apply_edit(self, request):
        return self._mutations.apply_edit(request)

    def commit_operation_against_base(self, changes, *, agent_id: str = "", edit_type: str, description: str = ""):
        return self._mutations.commit_operation_against_base(
            changes, agent_id=agent_id, edit_type=edit_type, description=description,
        )

    def commit_specs_many(self, requests: Any):
        return self._mutations.commit_specs_many(requests)

    def list_folder_files(self, folder: str):
        return self._content.list_folder_files(folder)

    def write_file(self, specs, *, agent_id: str = "", description: str = ""):
        return self._mutations.write_file(specs, agent_id=agent_id, description=description)

    def edit_file(self, specs, *, agent_id: str = "", description: str = ""):
        return self._mutations.edit_file(specs, agent_id=agent_id, description=description)

    def rename_symbol(
        self, file_path: str, line: int, character: int, new_name: str, *,
        agent_id: str = "", description: str = "",
    ):
        plan = self.rename_symbol_plan(file_path, line, character, new_name)
        return self.commit_rename_plan(plan, agent_id=agent_id, description=description)

    def commit_rename_plan(self, plan, *, agent_id: str = "", description: str = ""):
        return self._mutations.commit_rename_plan(plan, agent_id=agent_id, description=description)

    def commit_rename_plans_many(self, requests: Any):
        return self._mutations.commit_rename_plans_many(requests)

    def delete_file(self, paths, *, agent_id: str = "", description: str = ""):
        return self._mutations.delete_file(paths, agent_id=agent_id, description=description)

    def move_file(self, specs, *, agent_id: str = "", description: str = ""):
        return self._mutations.move_file(specs, agent_id=agent_id, description=description)

    def undo_last_edit(self, file_path: str):
        return self._mutations.undo_last_edit(file_path)

    def status(self):
        return build_status(
            sandbox_id=self.sandbox_id,
            workspace_root=self.workspace_root,
            initialized=self.is_initialized,
            symbol_index=self.symbol_index,
            arbiter=self.arbiter,
            tree_cache=self.tree_cache,
            lsp_client=self.lsp_client,
            rename_cache_stats=self._rename_planner.cache_stats(),
            rename_preview_fast_fallbacks=self._rename_planner.fast_fallbacks,
        )

    def get_telemetry(self):
        return build_telemetry(
            symbol_index=self.symbol_index,
            arbiter=self.arbiter,
            lsp_client=self.lsp_client,
        )

    def dispose(self) -> None:
        """Cleanup all resources."""
        self.arbiter.cleanup_locks()
        self.time_machine.clear()
        try:
            self.lsp_client.close()
        except Exception:  # pragma: no cover - defensive
            logger.debug("lsp_client.close() failed during dispose", exc_info=True)
        logger.info("CodeIntelligenceService disposed for sandbox %s", self.sandbox_id)
