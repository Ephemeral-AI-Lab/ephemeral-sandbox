"""CodeIntelligenceService — per-sandbox orchestrator.

Manages all code intelligence primitives (TreeCache, SymbolIndex,
Arbiter, Ledger, TimeMachine, Patcher, LspClient, QueryRouter) in a
single sandbox. Thread-safe with per-sandbox creation locks.
"""

from __future__ import annotations

import logging
import threading
from typing import Any

from ephemeralos.services.code_intelligence.arbiter import Arbiter
from ephemeralos.services.code_intelligence.backend_protocol import (
    LspBackendAdapter,
    SymbolIndexBackendAdapter,
)
from ephemeralos.services.code_intelligence.ledger import Ledger
from ephemeralos.services.code_intelligence.lsp_client import LspClient
from ephemeralos.services.code_intelligence.patcher import Patcher
from ephemeralos.services.code_intelligence.query_router import IntelligenceQueryRouter
from ephemeralos.services.code_intelligence.symbol_index import SymbolIndex
from ephemeralos.services.code_intelligence.time_machine import TimeMachine
from ephemeralos.services.code_intelligence.tree_cache import TreeCache
from ephemeralos.services.code_intelligence.types import (
    CITelemetry,
    Diagnostic,
    EditRequest,
    EditResult,
    HoverResult,
    ReferenceInfo,
    SymbolInfo,
)

logger = logging.getLogger(__name__)


class CodeIntelligenceService:
    """Per-sandbox code intelligence runtime.

    Orchestrates all CI primitives and exposes a unified query/edit API.

    Parameters
    ----------
    sandbox_id:
        The sandbox this service is bound to.
    workspace_root:
        Root directory for indexing and path validation.
    sandbox:
        Optional Daytona sandbox object for remote operations.
    """

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
        self._init_lock = threading.Lock()

        # Core components
        self.tree_cache = TreeCache(
            on_change=self._on_tree_change,
        )
        self.symbol_index = SymbolIndex(
            workspace_root=workspace_root,
        )
        self.arbiter = Arbiter(
            workspace_root=workspace_root,
            on_edit=self._on_edit_recorded,
        )
        self.ledger = Ledger()
        self.time_machine = TimeMachine()
        self.patcher = Patcher()
        self.lsp_client = LspClient(
            workspace_root=workspace_root,
            sandbox=sandbox,
        )

        # Query router with backend adapters
        self.query_router = IntelligenceQueryRouter()
        self.query_router.register(LspBackendAdapter(self.lsp_client))
        self.query_router.register(SymbolIndexBackendAdapter(self.symbol_index))

    # -- Initialization -------------------------------------------------------

    def ensure_initialized(self, wait: bool = True) -> bool:
        """Initialize symbol indexing. Returns True if ready."""
        with self._init_lock:
            if self._initialized:
                return True

        ready = self.symbol_index.ensure_built(wait=wait)
        self.lsp_client.ensure_ready()

        with self._init_lock:
            self._initialized = ready
        return ready

    @property
    def is_initialized(self) -> bool:
        with self._init_lock:
            return self._initialized

    # -- Query API ------------------------------------------------------------

    def find_definitions(
        self, file_path: str, symbol: str, line: int = 0, character: int = 0,
    ) -> list[SymbolInfo]:
        """Find symbol definitions."""
        return self.query_router.find_definitions(file_path, symbol, line, character)

    def find_references(
        self, file_path: str, symbol: str, line: int = 0, character: int = 0,
    ) -> list[ReferenceInfo]:
        """Find all references to a symbol."""
        return self.query_router.find_references(file_path, symbol, line, character)

    def hover(self, file_path: str, line: int, character: int) -> HoverResult | None:
        """Get hover information."""
        return self.query_router.hover(file_path, line, character)

    def diagnostics(self, file_path: str) -> list[Diagnostic]:
        """Get diagnostics for a file."""
        return self.query_router.diagnostics(file_path)

    def query_symbols(self, query: str) -> list[SymbolInfo]:
        """Search for symbols by name."""
        return self.symbol_index.find(query)

    # -- Edit API -------------------------------------------------------------

    def apply_edit(self, request: EditRequest) -> EditResult:
        """Apply an OCC-coordinated edit.

        1. Acquire per-file lock
        2. Validate edit token (if provided)
        3. Save snapshot for undo
        4. Apply edit via patcher
        5. Record in ledger
        6. Refresh symbol index
        7. Release lock
        """
        file_path = request.file_path

        if not self.arbiter.acquire_file_lock(file_path):
            return EditResult(
                success=False,
                file_path=file_path,
                message="Could not acquire file lock (timeout)",
                conflict=True,
            )

        try:
            # Read current content
            try:
                from pathlib import Path
                current = Path(file_path).read_text(encoding="utf-8")
            except Exception as exc:
                return EditResult(
                    success=False,
                    file_path=file_path,
                    message=f"Cannot read file: {exc}",
                )

            # Save snapshot for undo
            self.time_machine.save(file_path, current)

            # Apply edit
            from ephemeralos.services.code_intelligence.patcher import SearchReplaceEdit
            patch_result = self.patcher.apply_edits(
                current,
                [SearchReplaceEdit(old_text=request.old_text, new_text=request.new_text)],
            )

            if not patch_result.success:
                self.time_machine.discard_snapshot(file_path)
                return EditResult(
                    success=False,
                    file_path=file_path,
                    message="; ".join(patch_result.errors),
                )

            # Write back
            try:
                if self._sandbox:
                    self._sandbox.fs.upload_file(
                        file_path,
                        patch_result.content.encode("utf-8"),
                    )
                else:
                    Path(file_path).write_text(patch_result.content, encoding="utf-8")
            except Exception as exc:
                return EditResult(
                    success=False,
                    file_path=file_path,
                    message=f"Write failed: {exc}",
                )

            # Record
            import hashlib
            old_hash = hashlib.sha256(current.encode()).hexdigest()[:16]
            new_hash = hashlib.sha256(patch_result.content.encode()).hexdigest()[:16]

            self.ledger.record(
                file_path=file_path,
                agent_id=request.agent_id,
                edit_type="edit",
                old_hash=old_hash,
                new_hash=new_hash,
                description=request.description,
            )

            gen = self.arbiter.record_edit(file_path, request.agent_id)

            # Refresh caches
            self.tree_cache.put_content(file_path, patch_result.content)
            self.symbol_index.refresh(file_path, patch_result.content)
            self.lsp_client.invalidate(file_path)

            return EditResult(
                success=True,
                file_path=file_path,
                message=f"Applied {patch_result.edits_applied} edit(s)",
                snapshot_id=str(gen),
            )

        finally:
            self.arbiter.release_file_lock(file_path)

    def undo_last_edit(self, file_path: str) -> EditResult:
        """Undo the last edit to a file via TimeMachine."""
        snapshot = self.time_machine.rollback(file_path)
        if snapshot is None:
            return EditResult(
                success=False,
                file_path=file_path,
                message="No snapshot available for undo",
            )

        try:
            if self._sandbox:
                self._sandbox.fs.upload_file(
                    file_path,
                    snapshot.content.encode("utf-8"),
                )
            else:
                from pathlib import Path
                Path(file_path).write_text(snapshot.content, encoding="utf-8")
        except Exception as exc:
            return EditResult(
                success=False,
                file_path=file_path,
                message=f"Undo write failed: {exc}",
            )

        # Refresh caches
        self.tree_cache.put_content(file_path, snapshot.content)
        self.symbol_index.refresh(file_path, snapshot.content)
        self.lsp_client.invalidate(file_path)

        return EditResult(
            success=True,
            file_path=file_path,
            message="Reverted to previous snapshot",
        )

    # -- Telemetry ------------------------------------------------------------

    def status(self) -> dict[str, Any]:
        """Return service status summary."""
        lsp_tel = self.lsp_client.telemetry
        return {
            "sandbox_id": self.sandbox_id,
            "initialized": self.is_initialized,
            "workspace_root": self.workspace_root,
            "tree_cache": self.tree_cache.stats,
            "symbol_index": {
                "built": self.symbol_index.is_built,
                "files": self.symbol_index.indexed_files,
                "symbols": self.symbol_index.size,
                "generation": self.symbol_index.generation,
            },
            "arbiter": self.arbiter.status(),
            "ledger": {"entries": self.ledger.entry_count},
            "lsp": {
                "connected": self.lsp_client.connected,
                "queries": lsp_tel.queries,
                "cache_hits": lsp_tel.cache_hits,
            },
        }

    def get_telemetry(self) -> CITelemetry:
        """Return structured telemetry."""
        cache_stats = self.tree_cache.stats
        lsp_tel = self.lsp_client.telemetry
        return CITelemetry(
            tree_cache_size=cache_stats["size"],
            tree_cache_hits=cache_stats["hits"],
            tree_cache_misses=cache_stats["misses"],
            symbol_index_size=self.symbol_index.size,
            symbol_index_generation=self.symbol_index.generation,
            indexed_files=self.symbol_index.indexed_files,
            lsp_connected=self.lsp_client.connected,
            lsp_query_count=lsp_tel.queries,
            lsp_cache_hits=lsp_tel.cache_hits,
            arbiter_active_edits=self.arbiter.active_edit_count,
            ledger_entry_count=self.ledger.entry_count,
        )

    # -- Cleanup --------------------------------------------------------------

    def dispose(self) -> None:
        """Cleanup all resources."""
        self.tree_cache.invalidate_all()
        self.arbiter.cleanup_locks()
        self.time_machine.clear()
        logger.info("CodeIntelligenceService disposed for sandbox %s", self.sandbox_id)

    # -- Callbacks ------------------------------------------------------------

    def _on_tree_change(self, file_path: str, old_hash: str, new_hash: str) -> None:
        """Called when tree cache detects a file change."""
        self.query_router.register_file_change(file_path)

    def _on_edit_recorded(self, file_path: str, agent_id: str, generation: int) -> None:
        """Called after arbiter records an edit."""
        pass  # Hook point for observers


# ---------------------------------------------------------------------------
# Global service registry — per-sandbox singleton management
# ---------------------------------------------------------------------------

_SERVICES: dict[str, CodeIntelligenceService] = {}
_SERVICES_LOCK = threading.Lock()
_CREATION_LOCKS: dict[str, threading.Lock] = {}


def get_code_intelligence(
    sandbox_id: str,
    workspace_root: str = "/workspace",
    sandbox: Any = None,
) -> CodeIntelligenceService:
    """Get or create a CI service for a sandbox."""
    with _SERVICES_LOCK:
        if sandbox_id in _SERVICES:
            return _SERVICES[sandbox_id]
        if sandbox_id not in _CREATION_LOCKS:
            _CREATION_LOCKS[sandbox_id] = threading.Lock()
        creation_lock = _CREATION_LOCKS[sandbox_id]

    with creation_lock:
        # Double-check after acquiring creation lock
        with _SERVICES_LOCK:
            if sandbox_id in _SERVICES:
                return _SERVICES[sandbox_id]

        service = CodeIntelligenceService(
            sandbox_id=sandbox_id,
            workspace_root=workspace_root,
            sandbox=sandbox,
        )
        with _SERVICES_LOCK:
            _SERVICES[sandbox_id] = service

        return service


def get_code_intelligence_if_exists(sandbox_id: str) -> CodeIntelligenceService | None:
    """Fetch an existing CI service without creating one."""
    with _SERVICES_LOCK:
        return _SERVICES.get(sandbox_id)


def dispose_code_intelligence(sandbox_id: str) -> None:
    """Dispose and remove a CI service."""
    with _SERVICES_LOCK:
        service = _SERVICES.pop(sandbox_id, None)
    if service:
        service.dispose()


def dispose_all_code_intelligence() -> None:
    """Dispose all CI services."""
    with _SERVICES_LOCK:
        services = list(_SERVICES.values())
        _SERVICES.clear()
    for service in services:
        service.dispose()


def get_all_services_status() -> dict[str, dict]:
    """Return status for all active services."""
    with _SERVICES_LOCK:
        services = dict(_SERVICES)
    return {sid: svc.status() for sid, svc in services.items()}
