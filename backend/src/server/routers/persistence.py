"""Persistence API routes — DB-backed sessions, agent runs, usage."""

from __future__ import annotations

from typing import TYPE_CHECKING

from fastapi import APIRouter
from fastapi.responses import JSONResponse

if TYPE_CHECKING:
    from ephemeralos.db.stores import AgentRunStore, SessionStore, UsageStore


# ---------------------------------------------------------------------------
# Router factory
# ---------------------------------------------------------------------------


def create_persistence_router(
    get_session: callable,
    session_store: SessionStore,
    agent_run_store: AgentRunStore,
    usage_store: UsageStore,
    model_store: object = None,
) -> APIRouter:
    """Build the persistence API router."""
    router = APIRouter(prefix="/api/db")

    def _db_available() -> bool:
        return session_store._session_factory is not None

    # -- sessions --------------------------------------------------------------

    @router.get("/sessions")
    async def list_db_sessions(cwd: str | None = None, limit: int = 20):
        if not _db_available():
            return JSONResponse(
                status_code=503,
                content={"error": "Database not configured"},
            )
        sessions = session_store.list_sessions(cwd=cwd, limit=limit)
        return JSONResponse(content={"sessions": sessions})

    @router.get("/sessions/{session_id}")
    async def get_db_session(session_id: str):
        if not _db_available():
            return JSONResponse(
                status_code=503,
                content={"error": "Database not configured"},
            )
        record = session_store.get(session_id)
        if record is None:
            return JSONResponse(status_code=404, content={"error": "Session not found"})
        return JSONResponse(content={
            "session_id": record.id,
            "cwd": record.cwd,
            "model": record.model,
            "summary": record.summary,
            "message_count": record.message_count,
            "usage": record.usage,
            "created_at": record.created_at.isoformat() if record.created_at else None,
            "updated_at": record.updated_at.isoformat() if record.updated_at else None,
        })

    # -- agent runs ------------------------------------------------------------

    @router.get("/sessions/{session_id}/runs")
    async def list_session_runs(session_id: str, limit: int = 50):
        if not _db_available():
            return JSONResponse(
                status_code=503,
                content={"error": "Database not configured"},
            )
        runs = agent_run_store.list_runs(session_id, limit=limit)
        return JSONResponse(content={"runs": runs})

    @router.get("/runs/{run_id}")
    async def get_run(run_id: str):
        if not _db_available():
            return JSONResponse(
                status_code=503,
                content={"error": "Database not configured"},
            )
        record = agent_run_store.get_run(run_id)
        if record is None:
            return JSONResponse(status_code=404, content={"error": "Run not found"})
        return JSONResponse(content={
            "id": record.id,
            "session_id": record.session_id,
            "agent_name": record.agent_name,
            "status": record.status,
            "input_query": record.input_query,
            "error": record.error,
            "event_count": record.event_count,
            "started_at": record.started_at.isoformat() if record.started_at else None,
            "finished_at": record.finished_at.isoformat() if record.finished_at else None,
        })

    @router.get("/runs/{run_id}/chunks")
    async def list_run_chunks(run_id: str, limit: int = 500):
        if not _db_available():
            return JSONResponse(
                status_code=503,
                content={"error": "Database not configured"},
            )
        chunks = agent_run_store.list_chunks(run_id, limit=limit)
        return JSONResponse(content={"chunks": chunks})

    # -- usage -----------------------------------------------------------------

    @router.get("/usage")
    async def get_usage(session_id: str | None = None):
        if not _db_available():
            return JSONResponse(
                status_code=503,
                content={"error": "Database not configured"},
            )
        if session_id:
            data = usage_store.get_session_usage(session_id)
        else:
            data = {"by_model": usage_store.get_usage_by_model()}
        return JSONResponse(content=data)

    @router.get("/usage/{session_id}")
    async def get_session_usage(session_id: str):
        if not _db_available():
            return JSONResponse(
                status_code=503,
                content={"error": "Database not configured"},
            )
        data = usage_store.get_session_usage(session_id)
        return JSONResponse(content=data)

    # -- health ----------------------------------------------------------------

    @router.get("/health")
    async def db_health():
        return JSONResponse(content={
            "database": "connected" if _db_available() else "not_configured",
        })

    return router
