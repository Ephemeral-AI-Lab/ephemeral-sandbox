"""Per-sandbox :class:`CodeIntelligenceService` orchestrator.

This module wires together the analysis, editing, and routing
components. Heavy concerns live in their own modules:

* OCC write pipeline   → :mod:`code_intelligence.editing.write_coordinator`
* File IO              → :mod:`code_intelligence.routing.content_manager`
* Registry lifecycle   → :mod:`code_intelligence.routing.registry`

The registry helpers are re-exported from this module for backwards
compatibility with callers that import them from ``routing.service``.
"""

from __future__ import annotations

import logging
import threading
import time
from collections.abc import Sequence
from typing import Any

from team._path_utils import normalize_scope_paths, scope_paths_overlap

from code_intelligence.analysis.symbol_index import SymbolIndex
from code_intelligence.analysis.tree_cache import TreeCache
from code_intelligence.editing.arbiter import Arbiter
from code_intelligence.editing.patcher import Patcher
from code_intelligence.editing.time_machine import TimeMachine
from code_intelligence.editing.write_coordinator import WriteCoordinator, content_hash
from code_intelligence.lsp.client import LspClient
from code_intelligence.routing.backend_protocol import (
    LspBackendAdapter,
    SymbolIndexBackendAdapter,
)
from code_intelligence.routing.content_manager import ContentManager
from code_intelligence.routing.query_router import IntelligenceQueryRouter
from code_intelligence.routing.registry import (
    dispose_all_code_intelligence,
    dispose_code_intelligence,
    get_all_services_status,
    get_code_intelligence,
    get_code_intelligence_if_exists,
)
from code_intelligence.types import (
    CITelemetry,
    Diagnostic,
    EditRequest,
    EditResult,
    HoverResult,
    MultiEditResult,
    PreparedWrite,
    ReferenceInfo,
    SemanticFileChange,
    SemanticRenamePlan,
    SymbolInfo,
    WriteRequest,
)

__all__ = [
    "CodeIntelligenceService",
    "dispose_all_code_intelligence",
    "dispose_code_intelligence",
    "get_all_services_status",
    "get_code_intelligence",
    "get_code_intelligence_if_exists",
]

logger = logging.getLogger(__name__)
_DEFAULT_SCOPE_RECENT_SECONDS = 300.0


class CodeIntelligenceService:
    """Orchestrates code intelligence queries and edits for one sandbox."""

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

    # -- Initialization -------------------------------------------------------

    def ensure_initialized(self, wait: bool = True) -> bool:
        """Initialize symbol indexing + LSP. Returns True once ready."""
        with self._init_lock:
            if self._initialized:
                return True

        ready = self.symbol_index.ensure_built(wait=wait)
        lsp_ready = self.lsp_client.ensure_ready()
        if (
            self._sandbox is not None
            and (not lsp_ready.get("python") or not lsp_ready.get("typescript"))
            and not self._lsp_bootstrap_attempted
        ):
            self._lsp_bootstrap_attempted = True
            self.lsp_client.ensure_ready(install_missing=True)

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

    # -- Sandbox binding ------------------------------------------------------

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
        self._content.bind_sandbox(sandbox)

    # -- Query API ------------------------------------------------------------

    def find_definitions(
        self, file_path: str, symbol: str, line: int = 0, character: int = 0,
    ) -> list[SymbolInfo]:
        return self.query_router.find_definitions(file_path, symbol, line, character)

    def find_references(
        self, file_path: str, symbol: str, line: int = 0, character: int = 0,
    ) -> list[ReferenceInfo]:
        return self.query_router.find_references(file_path, symbol, line, character)

    def hover(self, file_path: str, line: int, character: int) -> HoverResult | None:
        return self.query_router.hover(file_path, line, character)

    def diagnostics(self, file_path: str) -> list[Diagnostic]:
        return self.query_router.diagnostics(file_path)

    def query_symbols(self, query: str) -> list[SymbolInfo]:
        return self.symbol_index.find(query)

    def rename_symbol_plan(
        self, file_path: str, line: int, character: int, new_name: str,
    ) -> SemanticRenamePlan:
        """Build a :class:`SemanticRenamePlan` for an OCC batch commit.

        Snapshots :attr:`arbiter.generation` *before* invoking Jedi so the
        batch commit can detect foreign edits that landed during planning.
        For each affected file, the file's current content is captured as
        the per-file OCC base; the generation gate covers the race window
        between Jedi's read and this capture.
        """
        gen_before = self.arbiter.generation
        final_by_path = self.lsp_client.rename_symbol(
            file_path, int(line), int(character), new_name,
        )
        changes: list[SemanticFileChange] = []
        try:
            base_by_path = self._content.read_many(
                list(final_by_path.keys()),
                allow_missing=True,
            )
        except Exception:  # pragma: no cover - defensive I/O
            base_by_path = {}
        for path, final_content in final_by_path.items():
            base_content, existed = base_by_path.get(path, ("", False))
            # Missing files are skipped: Jedi would not have produced a
            # rewrite against a file it could not see.
            if not existed and not base_content:
                continue
            changes.append(
                SemanticFileChange(
                    file_path=path,
                    base_content=base_content,
                    base_hash=content_hash(base_content),
                    final_content=final_content,
                ),
            )
        return SemanticRenamePlan(
            new_name=new_name,
            origin=(file_path, int(line), int(character)),
            arbiter_generation=gen_before,
            changes=tuple(changes),
        )

    # -- Edit API (delegated) -------------------------------------------------

    def apply_edit(self, request: EditRequest) -> EditResult:
        return self._write_coordinator.apply_edit(request)

    def apply_write(self, request: WriteRequest) -> EditResult:
        return self._write_coordinator.apply_write(request)

    def prepare_write(
        self,
        file_path: str,
        *,
        agent_id: str = "",
        expected_hash: str = "",
        allow_missing: bool = False,
    ) -> PreparedWrite | EditResult:
        return self._write_coordinator.prepare_write(
            file_path,
            agent_id=agent_id,
            expected_hash=expected_hash,
            allow_missing=allow_missing,
        )

    def commit_prepared_write(
        self,
        prepared: PreparedWrite,
        new_content: str,
        *,
        edit_type: str,
        description: str = "",
        message: str = "Wrote file",
    ) -> EditResult:
        return self._write_coordinator.commit_prepared_write(
            prepared,
            new_content,
            edit_type=edit_type,
            description=description,
            message=message,
        )

    def refresh_prepared_write(self, prepared: PreparedWrite) -> PreparedWrite:
        return self._write_coordinator.refresh_prepared_write(prepared)

    def abort_prepared_write(self, prepared: PreparedWrite) -> None:
        self._write_coordinator.abort_prepared_write(prepared)

    def commit_change_against_base(
        self,
        file_path: str,
        *,
        base_content: str | None,
        final_content: str | None,
        agent_id: str = "",
        edit_type: str,
        description: str = "",
    ) -> EditResult:
        return self._write_coordinator.commit_change_against_base(
            file_path,
            base_content=base_content,
            final_content=final_content,
            agent_id=agent_id,
            edit_type=edit_type,
            description=description,
        )

    def commit_many_against_base(
        self,
        changes: Sequence[SemanticFileChange],
        *,
        agent_id: str = "",
        edit_type: str,
        description: str = "",
        expected_arbiter_generation: int | None = None,
    ) -> MultiEditResult:
        return self._write_coordinator.commit_many_against_base(
            changes,
            agent_id=agent_id,
            edit_type=edit_type,
            description=description,
            expected_arbiter_generation=expected_arbiter_generation,
        )

    def undo_last_edit(self, file_path: str) -> EditResult:
        return self._write_coordinator.undo_last_edit(file_path)

    # -- Edit intents ---------------------------------------------------------

    def publish_edit_intent(
        self,
        *,
        filepath: str,
        agent_id: str = "",
        coordination_plan_id: str | None = None,
        task_id: str | None = None,
        symbols: list[str] | tuple[str, ...] | None = None,
        scope: str = "file",
    ) -> str:
        return self.arbiter.publish_edit_intent(
            filepath,
            agent_id,
            coordination_plan_id=coordination_plan_id,
            task_id=task_id,
            symbols=symbols,
            scope=scope,
        )

    def heartbeat_edit_intent(self, intent_id: str) -> bool:
        return self.arbiter.heartbeat_edit_intent(intent_id)

    def release_edit_intent(self, intent_id: str) -> None:
        self.arbiter.release_edit_intent(intent_id)

    # -- Scope status --------------------------------------------------------

    def scope_status(
        self,
        scope_paths: list[str] | tuple[str, ...] | None,
        *,
        team_run_id: str | None = None,
        briefing_versions: list[dict[str, Any]] | None = None,
        context_pressure: dict[str, Any] | None = None,
        shared_context: list[dict[str, Any]] | None = None,
        baseline_packet: dict[str, Any] | None = None,
        recent_seconds: float = _DEFAULT_SCOPE_RECENT_SECONDS,
    ) -> dict[str, Any]:
        """Return the authoritative live coordination snapshot for *scope_paths*."""
        normalized = normalize_scope_paths(scope_paths)
        history_ready = getattr(self.arbiter, "initialized", False)

        recent_changes: list[dict[str, Any]] = []
        if history_ready:
            for entry in self.arbiter.recent_edits(
                seconds=recent_seconds,
                team_run_id=team_run_id,
            ):
                fp = str(entry.file_path or "")
                if _scope_excludes(fp, normalized):
                    continue
                recent_changes.append(
                    {
                        "file_path": fp,
                        "agent_run_id": str(entry.agent_run_id or ""),
                        "task_id": str(entry.task_id or ""),
                        "timestamp": entry.created_at.timestamp() if entry.created_at else 0.0,
                        "edit_type": str(entry.edit_type or ""),
                    }
                )
        recent_changes.sort(key=lambda item: (item["file_path"], item["timestamp"]))

        hotspots: list[dict[str, Any]] = []
        if history_ready:
            for fp, count in self.arbiter.hotspots(
                limit=25,
                team_run_id=team_run_id,
            ):
                fp_str = str(fp)
                if _scope_excludes(fp_str, normalized):
                    continue
                hotspots.append({"file_path": fp_str, "edit_count": int(count)})
                if len(hotspots) >= 10:
                    break

        return {
            "scope_paths": normalized,
            "arbiter_generation": self.arbiter.generation,
            "symbol_index_generation": self.symbol_index.generation,
            "recent_changes": recent_changes[:25],
            "active_reservations": [dict(item) for item in self.arbiter.active_reservations(normalized)][:25],
            "active_edit_intents": [dict(item) for item in self.arbiter.active_edit_intents(normalized)][:25],
            "hotspots": hotspots,
            "generated_at": time.time(),
        }

    # -- Telemetry ------------------------------------------------------------

    def status(self) -> dict[str, Any]:
        """Return service status summary."""
        lsp = self._lsp_telemetry_fields()
        return {
            "sandbox_id": self.sandbox_id,
            "initialized": self.is_initialized,
            "workspace_root": self.workspace_root,
            "symbol_index": {
                "built": self.symbol_index.is_built,
                "files": self.symbol_index.indexed_files,
                "symbols": self.symbol_index.size,
                "generation": self.symbol_index.generation,
            },
            "arbiter": self.arbiter.status(),
            "edit_buffer": {
                "entries": self.arbiter.metrics.total_edits,
                "generation": self.arbiter.generation,
            },
            "lsp": lsp,
        }

    def get_telemetry(self) -> CITelemetry:
        lsp = self._lsp_telemetry_fields()
        return CITelemetry(
            symbol_index_size=self.symbol_index.size,
            symbol_index_generation=self.symbol_index.generation,
            indexed_files=self.symbol_index.indexed_files,
            lsp_connected=lsp["connected"],
            lsp_query_count=lsp["queries"],
            lsp_cache_hits=lsp["cache_hits"],
            arbiter_active_edits=self.arbiter.active_edit_count,
            total_edits=self.arbiter.metrics.total_edits,
        )

    def _lsp_telemetry_fields(self) -> dict[str, Any]:
        tel = self.lsp_client.telemetry
        return {
            "connected": self.lsp_client.connected,
            "queries": tel.queries,
            "cache_hits": tel.cache_hits,
            "worker_successes": tel.worker_successes,
            "worker_fallbacks": tel.worker_fallbacks,
            "worker_errors": tel.worker_errors,
        }

    # -- Cleanup --------------------------------------------------------------

    def dispose(self) -> None:
        """Cleanup all resources."""
        self.arbiter.cleanup_locks()
        self.time_machine.clear()
        try:
            self.lsp_client.close()
        except Exception:  # pragma: no cover - defensive
            logger.debug("lsp_client.close() failed during dispose", exc_info=True)
        logger.info("CodeIntelligenceService disposed for sandbox %s", self.sandbox_id)


def _scope_excludes(file_path: str, normalized_scope: list[str]) -> bool:
    """True if *normalized_scope* is non-empty and *file_path* does not overlap any entry."""
    if not normalized_scope:
        return False
    return not any(scope_paths_overlap(file_path, scope) for scope in normalized_scope)
