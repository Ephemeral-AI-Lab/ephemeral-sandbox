"""Per-sandbox code intelligence runtime."""

from __future__ import annotations

import contextlib
import hashlib
import inspect
import logging
import threading
import time
from typing import Any

from team._path_utils import normalize_scope_paths, scope_paths_overlap
from code_intelligence.editing.arbiter import Arbiter
from code_intelligence.editing.merge import (
    detect_edit_window,
    merge_non_overlapping_edit,
)
from code_intelligence.routing.backend_protocol import (
    LspBackendAdapter,
    SymbolIndexBackendAdapter,
)
from code_intelligence.lsp.client import LspClient
from code_intelligence.editing.patcher import Patcher
from code_intelligence.routing.query_router import IntelligenceQueryRouter
from code_intelligence.analysis.symbol_index import SymbolIndex
from code_intelligence.editing.time_machine import TimeMachine
from code_intelligence.types import (
    CITelemetry,
    Diagnostic,
    EditRequest,
    EditResult,
    HoverResult,
    PreparedWrite,
    ReferenceInfo,
    SymbolInfo,
    WriteRequest,
)

logger = logging.getLogger(__name__)
_DEFAULT_SCOPE_RECENT_SECONDS = 300.0


def _content_hash(content: str) -> str:
    return hashlib.sha256(content.encode("utf-8")).hexdigest()[:16]


def _result(
    file_path: str,
    message: str,
    *,
    success: bool = False,
    conflict: bool = False,
    conflict_reason: str = "",
    snapshot_id: str = "",
) -> EditResult:
    """Build a normalized edit result payload."""
    return EditResult(
        success=success,
        file_path=file_path,
        message=message,
        conflict=conflict,
        conflict_reason=conflict_reason,
        snapshot_id=snapshot_id,
    )


def _rebind_service_sandbox(service: CodeIntelligenceService, sandbox: Any) -> None:
    """Refresh the sandbox handle carried by a cached CI service."""
    if sandbox is None:
        return
    service._sandbox = sandbox
    lsp = getattr(service, "lsp_client", None)
    if lsp is not None:
        old_sandbox = getattr(lsp, "_sandbox", None)
        lsp._sandbox = sandbox
        if old_sandbox is not sandbox:
            reset = getattr(lsp, "reset_backend_availability", None)
            if callable(reset):
                reset()


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
        self._init_lock = threading.Lock()

        self.symbol_index = SymbolIndex(workspace_root=workspace_root)

        # In-memory file change tracking.
        from team.persistence.file_change_store import FileChangeStore
        self.arbiter = Arbiter(workspace_root=workspace_root, file_change_store=FileChangeStore())

        self.time_machine = TimeMachine()
        self.patcher = Patcher()
        self.lsp_client = LspClient(workspace_root=workspace_root, sandbox=sandbox)
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
        self,
        file_path: str,
        symbol: str,
        line: int = 0,
        character: int = 0,
    ) -> list[SymbolInfo]:
        """Find symbol definitions."""
        return self.query_router.find_definitions(file_path, symbol, line, character)

    def find_references(
        self,
        file_path: str,
        symbol: str,
        line: int = 0,
        character: int = 0,
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
        5. Record edit in arbiter
        6. Refresh symbol index
        7. Release lock
        """
        from code_intelligence.editing.patcher import SearchReplaceEdit

        with self._prepared_write_guard(
            request.file_path,
            agent_id=request.agent_id,
            expected_hash=request.expected_hash,
        ) as prepared:
            if isinstance(prepared, EditResult):
                return prepared

            edit = SearchReplaceEdit(old_text=request.old_text, new_text=request.new_text)
            patch_result = self._attempt_patch(prepared, edit)
            if not patch_result.success:
                self.time_machine.discard_snapshot(request.file_path)
                return EditResult(
                    success=False,
                    file_path=request.file_path,
                    message="; ".join(patch_result.errors),
                )

            refreshed = self.refresh_prepared_write(prepared)
            if (
                refreshed.token_id != prepared.token_id
                or refreshed.current_hash != prepared.current_hash
            ):
                prepared = refreshed
                patch_result = self._attempt_patch(prepared, edit)
                if not patch_result.success:
                    self.time_machine.discard_snapshot(request.file_path)
                    return EditResult(
                        success=False,
                        file_path=request.file_path,
                        message=(
                            "Write precheck failed: search text no longer matches the latest file "
                            "content. Re-read the file and retry."
                        ),
                        conflict=True,
                        conflict_reason="version_mismatch",
                    )

            return self.commit_prepared_write(
                prepared,
                patch_result.content,
                edit_type="edit",
                description=request.description,
                message=f"Applied {patch_result.edits_applied} edit(s)",
            )

    def apply_write(self, request: WriteRequest) -> EditResult:
        """Apply an OCC-coordinated full-file write."""
        with self._prepared_write_guard(
            request.file_path,
            agent_id=request.agent_id,
            expected_hash=request.expected_hash,
            allow_missing=True,
        ) as prepared:
            if isinstance(prepared, EditResult):
                return prepared
            return self.commit_prepared_write(
                prepared,
                request.content,
                edit_type=request.edit_type,
                description=request.description,
                message="Wrote file",
            )

    def prepare_write(
        self,
        file_path: str,
        *,
        agent_id: str = "",
        expected_hash: str = "",
        allow_missing: bool = False,
    ) -> PreparedWrite | EditResult:
        """Capture a stable read snapshot and issue a write reservation token."""
        try:
            current, existed = self._read_content(file_path, allow_missing=allow_missing)
        except Exception as exc:
            return _result(file_path, f"Cannot read file: {exc}")

        current_hash = _content_hash(current)
        if expected_hash and current_hash != expected_hash:
            return _result(
                file_path,
                "Write precheck failed: file content changed since it was read. "
                "Re-read the file and retry.",
                conflict=True,
            )
        token = self.arbiter.issue_token(file_path, current_hash, agent_id)
        return PreparedWrite(
            file_path=file_path,
            token_id=token.token_id,
            current_content=current,
            current_hash=current_hash,
            agent_id=agent_id,
            existed=existed,
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
        """Commit a prepared write after validating the reservation is still current."""
        if not self.arbiter.acquire_file_lock(prepared.file_path):
            return _result(
                prepared.file_path,
                "Could not acquire file lock (timeout)",
                conflict=True,
                conflict_reason="lock_timeout",
            )

        try:
            ok, reason = self.arbiter.validate_token(
                prepared.token_id,
                file_path=prepared.file_path,
                content_hash=prepared.current_hash,
            )
            if not ok:
                return _result(
                    prepared.file_path,
                    f"Write precheck failed: {reason}",
                    conflict=True,
                    conflict_reason="stale_reservation",
                )

            try:
                current_now, _ = self._read_content(prepared.file_path, allow_missing=True)
            except Exception as exc:
                return _result(
                    prepared.file_path,
                    f"Cannot re-read file before commit: {exc}",
                )

            write_content, old_hash, conflict = self._resolve_pending_write(
                prepared,
                current_now,
                new_content,
            )
            if conflict is not None:
                return conflict

            self.time_machine.save(prepared.file_path, current_now)
            try:
                self._write_content(prepared.file_path, write_content)
            except Exception as exc:
                return _result(prepared.file_path, f"Write failed: {exc}")

            new_hash = _content_hash(write_content)
            gen = self.arbiter.record_edit(
                file_path=prepared.file_path,
                agent_id=prepared.agent_id,
                edit_type=edit_type,
                old_hash=old_hash,
                new_hash=new_hash,
                description=description,
            )
            self.symbol_index.refresh(prepared.file_path, write_content)
            self.lsp_client.invalidate(prepared.file_path)
            self.arbiter.release_token(prepared.token_id)
            return _result(
                prepared.file_path,
                message,
                success=True,
                snapshot_id=str(gen),
            )
        finally:
            self.arbiter.release_file_lock(prepared.file_path)

    def refresh_prepared_write(self, prepared: PreparedWrite) -> PreparedWrite:
        """Refresh a prepared write snapshot, issuing a new token when the file changed."""
        try:
            current, existed = self._read_content(prepared.file_path, allow_missing=True)
        except Exception:
            return prepared

        current_hash = _content_hash(current)
        if current_hash == prepared.current_hash and existed == prepared.existed:
            return prepared

        self.abort_prepared_write(prepared)
        token = self.arbiter.issue_token(prepared.file_path, current_hash, prepared.agent_id)
        return PreparedWrite(
            file_path=prepared.file_path,
            token_id=token.token_id,
            current_content=current,
            current_hash=current_hash,
            agent_id=prepared.agent_id,
            existed=existed,
            line_start=prepared.line_start,
            line_end=prepared.line_end,
            operation_type=prepared.operation_type,
        )

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
        """Publish an edit intent through the arbiter."""
        return self.arbiter.publish_edit_intent(
            filepath,
            agent_id,
            coordination_plan_id=coordination_plan_id,
            task_id=task_id,
            symbols=symbols,
            scope=scope,
        )

    def heartbeat_edit_intent(self, intent_id: str) -> bool:
        """Refresh an edit intent heartbeat."""
        return self.arbiter.heartbeat_edit_intent(intent_id)

    def release_edit_intent(self, intent_id: str) -> None:
        """Release an edit intent."""
        self.arbiter.release_edit_intent(intent_id)

    def abort_prepared_write(self, prepared: PreparedWrite) -> None:
        """Release any reservation still held for *prepared*."""
        ok, _ = self.arbiter.validate_token(
            prepared.token_id,
            file_path=prepared.file_path,
        )
        if ok:
            self.arbiter.release_token(prepared.token_id)

    def undo_last_edit(self, file_path: str) -> EditResult:
        """Undo the last edit to a file via TimeMachine."""
        snapshot = self.time_machine.rollback(file_path)
        if snapshot is None:
            return _result(file_path, "No snapshot available for undo")

        try:
            self._write_content(file_path, snapshot.content)
        except Exception as exc:
            return _result(file_path, f"Undo write failed: {exc}")

        # Refresh caches
        self.symbol_index.refresh(file_path, snapshot.content)
        self.lsp_client.invalidate(file_path)

        return _result(file_path, "Reverted to previous snapshot", success=True)

    def scope_status(
        self,
        scope_paths: list[str] | tuple[str, ...] | None,
        *,
        briefing_versions: list[dict[str, Any]] | None = None,
        context_pressure: dict[str, Any] | None = None,
        shared_context: list[dict[str, Any]] | None = None,
        baseline_packet: dict[str, Any] | None = None,
        recent_seconds: float = _DEFAULT_SCOPE_RECENT_SECONDS,
    ) -> dict[str, Any]:
        """Return the authoritative live coordination snapshot for *scope_paths*."""
        normalized = normalize_scope_paths(scope_paths)
        recent_changes = []
        store = self.arbiter.file_change_store
        if store is not None and getattr(store, "initialized", False):
            for entry in store.recent_edits(seconds=recent_seconds):
                fp = str(entry.file_path or "")
                if normalized and not any(
                    scope_paths_overlap(fp, scope) for scope in normalized
                ):
                    continue
                recent_changes.append(
                    {
                        "file_path": fp,
                        "agent_id": str(entry.agent_id or ""),
                        "timestamp": entry.created_at.timestamp() if entry.created_at else 0.0,
                        "edit_type": str(entry.edit_type or ""),
                    }
                )
        recent_changes.sort(key=lambda item: (item["file_path"], item["timestamp"]))
        active_reservations = self.arbiter.active_reservations(normalized)
        active_edit_intents = self.arbiter.active_edit_intents(normalized)
        hotspots = []
        if store is not None and getattr(store, "initialized", False):
            for fp, count in store.hotspots(limit=25):
                fp = str(fp)
                if normalized and not any(
                    scope_paths_overlap(fp, scope) for scope in normalized
                ):
                    continue
                hotspots.append({"file_path": fp, "edit_count": int(count)})
                if len(hotspots) >= 10:
                    break

        return {
            "scope_paths": normalized,
            "arbiter_generation": self.arbiter.generation,
            "symbol_index_generation": self.symbol_index.generation,
            "recent_changes": recent_changes[:25],
            "active_reservations": [dict(item) for item in active_reservations][:25],
            "active_edit_intents": [dict(item) for item in active_edit_intents][:25],
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
        """Return structured telemetry."""
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

    # -- Cleanup --------------------------------------------------------------

    def dispose(self) -> None:
        """Cleanup all resources."""
        self.arbiter.cleanup_locks()
        self.time_machine.clear()
        logger.info("CodeIntelligenceService disposed for sandbox %s", self.sandbox_id)

    # -- Private helpers ------------------------------------------------------

    @contextlib.contextmanager
    def _prepared_write_guard(
        self,
        file_path: str,
        *,
        agent_id: str = "",
        expected_hash: str = "",
        allow_missing: bool = False,
    ):
        """Context manager that prepares a write and always aborts the token on exit.

        Yields the PreparedWrite (or an EditResult on early failure).  The
        caller must check ``isinstance(value, EditResult)`` and return it
        immediately when true — the guard still cleans up safely in that case.
        """
        prepared = self.prepare_write(
            file_path,
            agent_id=agent_id,
            expected_hash=expected_hash,
            allow_missing=allow_missing,
        )
        try:
            yield prepared
        finally:
            if not isinstance(prepared, EditResult):
                self.abort_prepared_write(prepared)

    def _attempt_patch(self, prepared: "PreparedWrite", edit: Any) -> Any:
        """Run the patcher against *prepared*'s current content for a single edit."""
        return self.patcher.apply_edits(prepared.current_content, [edit])

    def _lsp_telemetry_fields(self) -> dict[str, Any]:
        """Return the three LSP telemetry values shared by status() and get_telemetry()."""
        tel = self.lsp_client.telemetry
        return {
            "connected": self.lsp_client.connected,
            "queries": tel.queries,
            "cache_hits": tel.cache_hits,
        }

    def _resolve_pending_write(
        self,
        prepared: PreparedWrite,
        current_now: str,
        requested_content: str,
    ) -> tuple[str, str, EditResult | None]:
        """Merge a prepared write with the latest file content when possible."""
        current_hash = _content_hash(current_now)
        if current_hash == prepared.current_hash:
            return requested_content, prepared.current_hash, None

        line_start = prepared.line_start
        line_end = prepared.line_end
        operation_type = prepared.operation_type or "replace"
        if line_start is None:
            line_start, line_end, operation_type = detect_edit_window(
                prepared.current_content,
                requested_content,
            )

        if prepared.existed and line_start is not None:
            merged_content = merge_non_overlapping_edit(
                original_content=prepared.current_content,
                new_content=requested_content,
                current_content=current_now,
                line_start=line_start,
                line_end=line_end,
                operation_type=operation_type,
            )
            if merged_content is not None:
                return merged_content, current_hash, None
            return (
                "",
                current_hash,
                _result(
                    prepared.file_path,
                    "Write precheck failed: file content changed in an overlapping "
                    "or unsupported range. Re-read the file and retry.",
                    conflict=True,
                    conflict_reason="overlapping_range",
                ),
            )

        return (
            "",
            current_hash,
            _result(
                prepared.file_path,
                "Write precheck failed: file content changed before commit. "
                "Re-read the file and retry.",
                conflict=True,
                conflict_reason="version_mismatch",
            ),
        )

    def _write_content(self, file_path: str, content: str) -> None:
        """Write content locally or to the attached sandbox."""
        from pathlib import Path

        if self._sandbox:
            self._write_content_to_sandbox(file_path, content.encode("utf-8"))
            return
        Path(file_path).write_text(content, encoding="utf-8")

    def _write_content_to_sandbox(self, file_path: str, payload: bytes) -> None:
        """Handle both known upload_file argument orders exposed by sandboxes."""
        try:
            result = self._sandbox.fs.upload_file(
                payload,
                file_path,
            )
            self._resolve(result)
        except (AttributeError, TypeError) as exc:
            if "decode" not in str(exc) and "bytes-like object" not in str(exc):
                raise
            result = self._sandbox.fs.upload_file(
                file_path,
                payload,
            )
            self._resolve(result)

    def _read_content(
        self,
        file_path: str,
        *,
        allow_missing: bool = False,
    ) -> tuple[str, bool]:
        """Read content locally or from the attached sandbox."""
        from pathlib import Path

        if self._sandbox:
            try:
                raw = self._resolve(self._sandbox.fs.download_file(file_path))
            except Exception as exc:
                if allow_missing and self._is_missing_error(exc):
                    return "", False
                raise
            if isinstance(raw, bytes):
                return raw.decode("utf-8"), True
            return str(raw), True

        path = Path(file_path)
        if not path.exists():
            if allow_missing:
                return "", False
            raise FileNotFoundError(file_path)
        return path.read_text(encoding="utf-8"), True

    @staticmethod
    def _is_missing_error(exc: Exception) -> bool:
        text = str(exc).lower()
        if isinstance(exc, FileNotFoundError):
            return True
        return "not found" in text or "no such file" in text or "does not exist" in text

    @staticmethod
    def _resolve(result: Any) -> Any:
        """If *result* is awaitable, run it synchronously."""
        import asyncio
        import concurrent.futures

        if inspect.isawaitable(result):
            try:
                loop = asyncio.get_running_loop()
            except RuntimeError:
                loop = None
            if loop and loop.is_running():
                with concurrent.futures.ThreadPoolExecutor(max_workers=1) as pool:
                    return pool.submit(asyncio.run, result).result()
            return asyncio.run(result)
        return result


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
    existing: CodeIntelligenceService | None = None
    with _SERVICES_LOCK:
        existing = _SERVICES.get(sandbox_id)
        if existing is not None and existing.workspace_root == workspace_root:
            _rebind_service_sandbox(existing, sandbox)
            return existing
        if sandbox_id not in _CREATION_LOCKS:
            _CREATION_LOCKS[sandbox_id] = threading.Lock()
        creation_lock = _CREATION_LOCKS[sandbox_id]

    with creation_lock:
        # Double-check after acquiring creation lock
        with _SERVICES_LOCK:
            existing = _SERVICES.get(sandbox_id)
            if existing is not None and existing.workspace_root == workspace_root:
                _rebind_service_sandbox(existing, sandbox)
                return existing
            if existing is not None:
                _SERVICES.pop(sandbox_id, None)

        if existing is not None:
            existing.dispose()

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
