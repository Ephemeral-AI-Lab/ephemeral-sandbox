"""Code Intelligence API router — query, edit, and stream endpoints."""

from __future__ import annotations

import logging
from typing import Any

from fastapi import APIRouter, HTTPException, Query
from pydantic import BaseModel, Field

logger = logging.getLogger(__name__)

router = APIRouter(prefix="/api/code_intelligence", tags=["code_intelligence"])


# ---------------------------------------------------------------------------
# Request / Response models
# ---------------------------------------------------------------------------

class EditRequest(BaseModel):
    file_path: str
    old_text: str
    new_text: str
    agent_id: str = ""
    description: str = ""


class SymbolQueryRequest(BaseModel):
    query: str
    kind: str = ""


class LspQueryRequest(BaseModel):
    file_path: str
    line: int
    character: int = 0
    symbol: str = ""


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def _get_service(sandbox_id: str) -> Any:
    """Get or create a CI service for a sandbox."""
    from ephemeralos.services.code_intelligence.service import get_code_intelligence
    return get_code_intelligence(sandbox_id)


def _get_service_if_exists(sandbox_id: str) -> Any:
    """Get existing CI service or raise 404."""
    from ephemeralos.services.code_intelligence.service import get_code_intelligence_if_exists
    service = get_code_intelligence_if_exists(sandbox_id)
    if service is None:
        raise HTTPException(404, f"No CI service for sandbox '{sandbox_id}'")
    return service


# ---------------------------------------------------------------------------
# Status endpoints
# ---------------------------------------------------------------------------

@router.get("/health")
async def health() -> dict:
    """Code intelligence health check."""
    from ephemeralos.services.code_intelligence.service import get_all_services_status
    statuses = get_all_services_status()
    return {"healthy": True, "active_services": len(statuses)}


@router.get("/{sandbox_id}/status")
async def status(sandbox_id: str) -> dict:
    """Get CI service status for a sandbox."""
    service = _get_service_if_exists(sandbox_id)
    return service.status()


@router.post("/initialize/{sandbox_id}")
async def initialize(sandbox_id: str, workspace_root: str = "/workspace") -> dict:
    """Initialize CI service for a sandbox."""
    service = _get_service(sandbox_id)
    service.workspace_root = workspace_root
    ready = service.ensure_initialized(wait=True)
    return {"sandbox_id": sandbox_id, "initialized": ready}


# ---------------------------------------------------------------------------
# Query endpoints
# ---------------------------------------------------------------------------

@router.get("/{sandbox_id}/query/definitions")
async def query_definitions(
    sandbox_id: str,
    file_path: str = Query(...),
    symbol: str = Query(""),
    line: int = Query(0),
    character: int = Query(0),
) -> list[dict]:
    """Find symbol definitions."""
    service = _get_service_if_exists(sandbox_id)
    results = service.find_definitions(file_path, symbol, line, character)
    return [
        {
            "name": s.name,
            "kind": s.kind.value if hasattr(s.kind, "value") else str(s.kind),
            "file_path": s.file_path,
            "line": s.line,
            "character": s.character,
            "signature": s.signature,
        }
        for s in results
    ]


@router.get("/{sandbox_id}/query/references")
async def query_references(
    sandbox_id: str,
    file_path: str = Query(...),
    symbol: str = Query(""),
    line: int = Query(0),
    character: int = Query(0),
) -> list[dict]:
    """Find all references to a symbol."""
    service = _get_service_if_exists(sandbox_id)
    results = service.find_references(file_path, symbol, line, character)
    return [
        {
            "file_path": r.file_path,
            "line": r.line,
            "character": r.character,
            "text": r.text,
        }
        for r in results
    ]


@router.get("/{sandbox_id}/query/hover")
async def query_hover(
    sandbox_id: str,
    file_path: str = Query(...),
    line: int = Query(...),
    character: int = Query(0),
) -> dict | None:
    """Get hover information at a position."""
    service = _get_service_if_exists(sandbox_id)
    result = service.hover(file_path, line, character)
    if result is None:
        return None
    return {"content": result.content, "language": result.language}


@router.get("/{sandbox_id}/query/symbols")
async def query_symbols(
    sandbox_id: str,
    query: str = Query(...),
) -> list[dict]:
    """Search for symbols by name."""
    service = _get_service_if_exists(sandbox_id)
    results = service.query_symbols(query)
    return [
        {
            "name": s.name,
            "kind": s.kind.value if hasattr(s.kind, "value") else str(s.kind),
            "file_path": s.file_path,
            "line": s.line,
            "signature": s.signature,
        }
        for s in results[:100]
    ]


@router.get("/{sandbox_id}/query/diagnostics")
async def query_diagnostics(
    sandbox_id: str,
    file_path: str = Query(...),
) -> list[dict]:
    """Get diagnostics for a file."""
    service = _get_service_if_exists(sandbox_id)
    results = service.diagnostics(file_path)
    return [
        {
            "file_path": d.file_path,
            "line": d.line,
            "character": d.character,
            "severity": d.severity.value if hasattr(d.severity, "value") else str(d.severity),
            "message": d.message,
            "source": d.source,
        }
        for d in results
    ]


# ---------------------------------------------------------------------------
# Edit endpoints
# ---------------------------------------------------------------------------

@router.post("/{sandbox_id}/edit")
async def apply_edit(sandbox_id: str, request: EditRequest) -> dict:
    """Apply an OCC-coordinated edit."""
    service = _get_service_if_exists(sandbox_id)
    from ephemeralos.services.code_intelligence.types import EditRequest as CIEditRequest
    result = service.apply_edit(CIEditRequest(
        file_path=request.file_path,
        old_text=request.old_text,
        new_text=request.new_text,
        agent_id=request.agent_id,
        description=request.description,
    ))
    return {
        "success": result.success,
        "file_path": result.file_path,
        "message": result.message,
        "conflict": result.conflict,
    }


@router.post("/{sandbox_id}/undo")
async def undo_edit(sandbox_id: str, file_path: str = Query(...)) -> dict:
    """Undo the last edit to a file."""
    service = _get_service_if_exists(sandbox_id)
    result = service.undo_last_edit(file_path)
    return {
        "success": result.success,
        "file_path": result.file_path,
        "message": result.message,
    }


# ---------------------------------------------------------------------------
# Telemetry
# ---------------------------------------------------------------------------

@router.get("/{sandbox_id}/telemetry")
async def telemetry(sandbox_id: str) -> dict:
    """Get CI telemetry for a sandbox."""
    service = _get_service_if_exists(sandbox_id)
    tel = service.get_telemetry()
    return {
        "tree_cache_size": tel.tree_cache_size,
        "tree_cache_hits": tel.tree_cache_hits,
        "tree_cache_misses": tel.tree_cache_misses,
        "symbol_index_size": tel.symbol_index_size,
        "symbol_index_generation": tel.symbol_index_generation,
        "indexed_files": tel.indexed_files,
        "lsp_connected": tel.lsp_connected,
        "lsp_query_count": tel.lsp_query_count,
        "arbiter_active_edits": tel.arbiter_active_edits,
        "ledger_entry_count": tel.ledger_entry_count,
    }


# ---------------------------------------------------------------------------
# Lifecycle
# ---------------------------------------------------------------------------

@router.post("/{sandbox_id}/dispose")
async def dispose_service(sandbox_id: str) -> dict:
    """Dispose CI service for a sandbox."""
    from ephemeralos.services.code_intelligence.service import dispose_code_intelligence
    dispose_code_intelligence(sandbox_id)
    return {"disposed": sandbox_id}
