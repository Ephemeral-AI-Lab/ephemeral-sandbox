"""CodeIntelligenceGateway — stable public facade.

Provides a cached, high-level interface to the CodeIntelligenceService.
Consumers should use the gateway instead of the service directly.
Supports lazy initialization and parameter updates.
"""

from __future__ import annotations

import logging
import threading
import time
from typing import Any

from ephemeralos.services.code_intelligence.service import (
    CodeIntelligenceService,
    get_code_intelligence,
)
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


class CodeIntelligenceGateway:
    """Stable public interface to a per-sandbox CodeIntelligenceService.

    The gateway manages lazy initialization, parameter updates, and
    provides query/edit/cache mixins.

    Parameters
    ----------
    sandbox_id:
        The sandbox to bind to.
    workspace_root:
        Override workspace root directory.
    sandbox:
        Optional Daytona sandbox object.
    """

    def __init__(
        self,
        sandbox_id: str,
        workspace_root: str = "/workspace",
        sandbox: Any = None,
    ) -> None:
        self.sandbox_id = sandbox_id
        self._workspace_root = workspace_root
        self._sandbox = sandbox
        self._service: CodeIntelligenceService | None = None
        self._lock = threading.Lock()

    # -- Service resolution ---------------------------------------------------

    def _resolve_service(self, require: bool = True) -> CodeIntelligenceService | None:
        """Get or create the underlying service."""
        with self._lock:
            if self._service is not None:
                return self._service

        service = get_code_intelligence(
            sandbox_id=self.sandbox_id,
            workspace_root=self._workspace_root,
            sandbox=self._sandbox,
        )
        with self._lock:
            self._service = service
        return service

    def ensure_initialized(self, wait: bool = True) -> bool:
        """Initialize the underlying service."""
        service = self._resolve_service()
        if service is None:
            return False
        return service.ensure_initialized(wait=wait)

    # -- Query mixin ----------------------------------------------------------

    def find_definitions(
        self, file_path: str, symbol: str, line: int = 0, character: int = 0,
    ) -> list[SymbolInfo]:
        service = self._resolve_service()
        return service.find_definitions(file_path, symbol, line, character) if service else []

    def find_references(
        self, file_path: str, symbol: str, line: int = 0, character: int = 0,
    ) -> list[ReferenceInfo]:
        service = self._resolve_service()
        return service.find_references(file_path, symbol, line, character) if service else []

    def hover(self, file_path: str, line: int, character: int) -> HoverResult | None:
        service = self._resolve_service()
        return service.hover(file_path, line, character) if service else None

    def diagnostics(self, file_path: str) -> list[Diagnostic]:
        service = self._resolve_service()
        return service.diagnostics(file_path) if service else []

    def query_symbols(self, query: str) -> list[SymbolInfo]:
        service = self._resolve_service()
        return service.query_symbols(query) if service else []

    # -- Edit mixin -----------------------------------------------------------

    def apply_edit(self, request: EditRequest) -> EditResult:
        service = self._resolve_service()
        if service is None:
            return EditResult(success=False, file_path=request.file_path, message="CI not available")
        return service.apply_edit(request)

    def undo_last_edit(self, file_path: str) -> EditResult:
        service = self._resolve_service()
        if service is None:
            return EditResult(success=False, file_path=file_path, message="CI not available")
        return service.undo_last_edit(file_path)

    # -- Cache mixin ----------------------------------------------------------

    def prime_cache(self, file_paths: list[str]) -> int:
        """Pre-parse files into the tree cache."""
        service = self._resolve_service()
        if service is None:
            return 0
        return service.tree_cache.prime_cache(file_paths)

    def invalidate(self, file_path: str) -> None:
        """Invalidate caches for a file."""
        service = self._resolve_service()
        if service:
            service.tree_cache.invalidate(file_path)
            service.lsp_client.invalidate(file_path)

    def invalidate_all(self) -> None:
        """Invalidate all caches."""
        service = self._resolve_service()
        if service:
            service.tree_cache.invalidate_all()

    # -- Telemetry ------------------------------------------------------------

    def get_telemetry(self) -> CITelemetry:
        service = self._resolve_service()
        return service.get_telemetry() if service else CITelemetry()

    def status(self) -> dict[str, Any]:
        service = self._resolve_service()
        return service.status() if service else {"sandbox_id": self.sandbox_id, "initialized": False}

    # -- Component access (for advanced consumers) ----------------------------

    @property
    def tree_cache(self) -> Any:
        service = self._resolve_service()
        return service.tree_cache if service else None

    @property
    def symbol_index(self) -> Any:
        service = self._resolve_service()
        return service.symbol_index if service else None

    @property
    def arbiter(self) -> Any:
        service = self._resolve_service()
        return service.arbiter if service else None

    @property
    def ledger(self) -> Any:
        service = self._resolve_service()
        return service.ledger if service else None

    @property
    def patcher(self) -> Any:
        service = self._resolve_service()
        return service.patcher if service else None

    @property
    def time_machine(self) -> Any:
        service = self._resolve_service()
        return service.time_machine if service else None

    # -- Lifecycle ------------------------------------------------------------

    def dispose(self) -> None:
        """Dispose the underlying service."""
        service = self._resolve_service(require=False)
        if service:
            service.dispose()
        with self._lock:
            self._service = None

    def update_params(
        self,
        sandbox: Any = None,
        workspace_root: str | None = None,
    ) -> None:
        """Update gateway parameters (e.g. after sandbox restart)."""
        if sandbox is not None:
            self._sandbox = sandbox
        if workspace_root is not None:
            self._workspace_root = workspace_root
        # Reset service so next call picks up new params
        with self._lock:
            self._service = None


# ---------------------------------------------------------------------------
# Gateway cache — LRU with bounded size
# ---------------------------------------------------------------------------

_gateway_cache: dict[str, CodeIntelligenceGateway] = {}
_gateway_cache_lock = threading.Lock()
_gateway_access: dict[str, float] = {}
_MAX_GATEWAY_CACHE_SIZE = 20


def get_code_intelligence_gateway(
    sandbox_id: str,
    workspace_root: str = "/workspace",
    sandbox: Any = None,
) -> CodeIntelligenceGateway:
    """Get or create a cached gateway for a sandbox."""
    with _gateway_cache_lock:
        if sandbox_id in _gateway_cache:
            gw = _gateway_cache[sandbox_id]
            _gateway_access[sandbox_id] = time.time()
            # Update params if provided
            if sandbox is not None:
                gw.update_params(sandbox=sandbox)
            if workspace_root:
                gw.update_params(workspace_root=workspace_root)
            return gw

        # Evict if at capacity
        while len(_gateway_cache) >= _MAX_GATEWAY_CACHE_SIZE:
            oldest_id = min(_gateway_access, key=_gateway_access.get)  # type: ignore[arg-type]
            evicted = _gateway_cache.pop(oldest_id, None)
            _gateway_access.pop(oldest_id, None)
            if evicted:
                evicted.dispose()

        gw = CodeIntelligenceGateway(
            sandbox_id=sandbox_id,
            workspace_root=workspace_root,
            sandbox=sandbox,
        )
        _gateway_cache[sandbox_id] = gw
        _gateway_access[sandbox_id] = time.time()
        return gw
