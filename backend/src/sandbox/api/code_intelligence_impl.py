"""``CodeIntelligenceApi`` implementation backed by ``CodeIntelligenceService``.

Phase 1 keeps the engine backend-hosted; this thin adapter exposes the
provider-neutral :class:`CodeIntelligenceApi` Protocol on top of an
existing ``CodeIntelligenceService`` instance. All sync→async bridging
is contained here via :func:`run_sync_in_executor`.

Transport-backed sandboxes now route through the daemon backend without
touching any tool callers — they only see :class:`CodeIntelligenceApi`.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any, ClassVar

from sandbox.api.models import (
    Diagnostic,
    DiagnosticsRequest,
    DiagnosticsResult,
    ReferencesRequest,
    ReferencesResult,
    SymbolDefinition,
    SymbolQueryRequest,
    SymbolQueryResult,
    SymbolReference,
    WorkspaceStatus,
    WorkspaceStructureRequest,
    WorkspaceStructureResult,
)


_SUPPORTED_EXTENSIONS = {".py", ".pyi"}


def _looks_like_file_query(query: str) -> bool:
    candidate = str(query or "").strip()
    if not candidate:
        return False
    if "/" in candidate or "\\" in candidate:
        return True
    lowered = candidate.lower()
    return any(lowered.endswith(ext) for ext in _SUPPORTED_EXTENSIONS)


def _kind_str(symbol: Any) -> str:
    kind = getattr(symbol, "kind", None)
    return getattr(kind, "value", None) or str(kind or "")


def _severity_str(diag: Any) -> str:
    sev = getattr(diag, "severity", None)
    value = getattr(sev, "value", None) or str(sev or "")
    return value if value in {"error", "warning", "information", "hint"} else "error"


def _to_symbol_definition(sym: Any) -> SymbolDefinition:
    return SymbolDefinition(
        name=str(getattr(sym, "name", "") or ""),
        kind=_kind_str(sym),
        file_path=str(getattr(sym, "file_path", "") or ""),
        line=int(getattr(sym, "line", 0) or 0),
        character=int(getattr(sym, "character", 0) or 0),
        signature=str(getattr(sym, "signature", "") or ""),
        container=str(getattr(sym, "container", "") or ""),
    )


def _to_symbol_reference(ref: Any) -> SymbolReference:
    return SymbolReference(
        file_path=str(getattr(ref, "file_path", "") or ""),
        line=int(getattr(ref, "line", 0) or 0),
        character=int(getattr(ref, "character", 0) or 0),
        text=str(getattr(ref, "text", "") or ""),
    )


def _to_diagnostic(diag: Any) -> Diagnostic:
    return Diagnostic(
        line=int(getattr(diag, "line", 0) or 0),
        character=int(getattr(diag, "character", 0) or 0),
        severity=_severity_str(diag),  # type: ignore[arg-type]
        message=str(getattr(diag, "message", "") or ""),
        source=str(getattr(diag, "source", "") or ""),
        code=str(getattr(diag, "code", "") or ""),
    )


class SvcCodeIntelligence:
    """``CodeIntelligenceApi`` implementation that wraps a ``CodeIntelligenceService``."""

    name: ClassVar[str] = "svc"

    def __init__(self, svc: Any) -> None:
        self._svc = svc

    async def status(self, sandbox_id: str) -> WorkspaceStatus:
        del sandbox_id
        from sandbox.client.async_bridge import run_sync_in_executor, use_sandbox_io_loop

        with use_sandbox_io_loop():
            raw = await run_sync_in_executor(self._svc.status)
        if not isinstance(raw, dict):
            raw = {}
        return WorkspaceStatus(
            sandbox_id=str(raw.get("sandbox_id", "") or ""),
            workspace_root=str(raw.get("workspace_root", "") or ""),
            initialized=bool(raw.get("initialized", False)),
            symbol_index=dict(raw.get("symbol_index") or {}),
            arbiter=dict(raw.get("arbiter") or {}),
            edit_buffer=dict(raw.get("edit_buffer") or {}),
            lsp=dict(raw.get("lsp") or {}),
            edit_hotspots=(
                dict(raw["edit_hotspots"])
                if isinstance(raw.get("edit_hotspots"), dict)
                else None
            ),
        )

    async def query_symbols(
        self, sandbox_id: str, request: SymbolQueryRequest,
    ) -> SymbolQueryResult:
        del sandbox_id
        if _looks_like_file_query(request.query):
            return await self._file_path_query(request)
        return await self._symbol_name_query(request)

    async def find_references(
        self, sandbox_id: str, request: ReferencesRequest,
    ) -> ReferencesResult:
        del sandbox_id
        from sandbox.client.async_bridge import run_sync_in_executor, use_sandbox_io_loop

        with use_sandbox_io_loop():
            raw_refs = await run_sync_in_executor(
                self._svc.find_references,
                request.file_path,
                request.symbol,
                request.line,
                request.character,
            )
        return ReferencesResult(
            references=tuple(_to_symbol_reference(r) for r in raw_refs or ()),
        )

    async def diagnostics(
        self, sandbox_id: str, request: DiagnosticsRequest,
    ) -> DiagnosticsResult:
        del sandbox_id
        from sandbox.client.async_bridge import run_sync_in_executor, use_sandbox_io_loop

        with use_sandbox_io_loop():
            raw_diags = await run_sync_in_executor(
                self._svc.diagnostics, request.file_path,
            )
        diagnostics = tuple(_to_diagnostic(d) for d in raw_diags or ())
        return DiagnosticsResult(
            diagnostics=diagnostics,
            clean=not diagnostics,
        )

    async def workspace_structure(
        self, sandbox_id: str, request: WorkspaceStructureRequest,
    ) -> WorkspaceStructureResult:
        del sandbox_id
        from sandbox.client.async_bridge import run_sync_in_executor, use_sandbox_io_loop

        si = getattr(self._svc, "symbol_index", None)
        workspace_root = str(getattr(self._svc, "workspace_root", "") or "")
        if si is None:
            return WorkspaceStructureResult(
                paths=(), source="none", workspace_root=workspace_root,
            )
        if not getattr(si, "is_built", False):
            try:
                with use_sandbox_io_loop():
                    await run_sync_in_executor(
                        si.ensure_built, True, 60.0,
                    )
            except Exception:
                pass
        if not hasattr(si, "paths_with_prefix"):
            return WorkspaceStructureResult(
                paths=(), source="none", workspace_root=workspace_root,
            )
        try:
            with use_sandbox_io_loop():
                indexed_paths = await run_sync_in_executor(
                    si.paths_with_prefix, request.path,
                )
        except Exception:
            return WorkspaceStructureResult(
                paths=(), source="none", workspace_root=workspace_root,
            )
        depth_filtered = self._depth_filter(
            indexed_paths or (),
            workspace_root=workspace_root,
            path_prefix=request.path,
            max_depth=request.max_depth,
        )
        if not depth_filtered:
            return WorkspaceStructureResult(
                paths=(), source="none", workspace_root=workspace_root,
            )
        return WorkspaceStructureResult(
            paths=tuple(depth_filtered),
            source="index",
            workspace_root=workspace_root,
        )

    # -- internals ---------------------------------------------------------

    async def _symbol_name_query(self, request: SymbolQueryRequest) -> SymbolQueryResult:
        from sandbox.client.async_bridge import run_sync_in_executor, use_sandbox_io_loop

        with use_sandbox_io_loop():
            raw_results = await run_sync_in_executor(
                self._svc.query_symbols, request.query,
            )
        if request.kind:
            kind_lower = request.kind.lower()
            raw_results = [
                s for s in raw_results or () if _kind_str(s) == kind_lower
            ]
        definitions = tuple(_to_symbol_definition(s) for s in raw_results or ())
        if not definitions:
            return SymbolQueryResult(
                definitions=(),
                confidence="none",
            )
        return SymbolQueryResult(
            definitions=definitions,
            confidence="" if not request.include_references else "unavailable",
        )

    async def _file_path_query(self, request: SymbolQueryRequest) -> SymbolQueryResult:
        from sandbox.client.async_bridge import run_sync_in_executor, use_sandbox_io_loop

        si = getattr(self._svc, "symbol_index", None)
        file_symbols_fn = getattr(si, "file_symbols", None) if si is not None else None
        if not callable(file_symbols_fn):
            return SymbolQueryResult(
                definitions=(),
                confidence="none",
                matched_file=request.query,
            )

        candidates = [request.query]
        suffix = Path(request.query).suffix
        if not suffix:
            candidates.extend([f"{request.query}.py", f"{request.query}/__init__.py"])

        matches: list[Any] = []
        matched_file = request.query
        for candidate in candidates:
            with use_sandbox_io_loop():
                hits = await run_sync_in_executor(file_symbols_fn, candidate)
            if hits:
                matches = list(hits)
                matched_file = candidate
                break

        if not matches:
            return SymbolQueryResult(
                definitions=(),
                confidence="none",
                matched_file=matched_file,
            )
        return SymbolQueryResult(
            definitions=tuple(_to_symbol_definition(s) for s in matches),
            confidence="file_symbols",
            matched_file=matched_file,
        )

    @staticmethod
    def _depth_filter(
        paths: Any,
        *,
        workspace_root: str,
        path_prefix: str,
        max_depth: int,
    ) -> list[str]:
        normalized_prefix = path_prefix.strip("/")
        depth_limit = max(0, int(max_depth))
        rendered: list[str] = []
        for raw in paths:
            rel_path = str(raw or "")
            if workspace_root and rel_path.startswith(workspace_root + "/"):
                rel_path = rel_path[len(workspace_root) + 1 :]
            rel_path = rel_path.lstrip("/")
            if not rel_path:
                continue
            relative_to_prefix = rel_path
            if normalized_prefix:
                if rel_path == normalized_prefix:
                    relative_to_prefix = ""
                elif rel_path.startswith(normalized_prefix + "/"):
                    relative_to_prefix = rel_path[len(normalized_prefix) + 1 :]
                else:
                    continue
            depth = (
                len([p for p in relative_to_prefix.split("/") if p])
                if relative_to_prefix
                else 0
            )
            if depth <= depth_limit:
                rendered.append(rel_path)
        return rendered


__all__ = ["SvcCodeIntelligence"]
