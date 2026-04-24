"""Persistence API routes — DB-backed sessions, agent runs, usage."""

from __future__ import annotations

from db.stores import AgentRunStore, SessionStore, UsageStore

from fastapi import APIRouter
from fastapi.responses import JSONResponse


# ---------------------------------------------------------------------------
# Router factory
# ---------------------------------------------------------------------------


def create_persistence_router(
    session_store: SessionStore,
    agent_run_store: AgentRunStore,
    usage_store: UsageStore,
) -> APIRouter:
    """Build the persistence API router."""
    router = APIRouter(prefix="/api/db")

    def _usage_for_runs(run_ids: list[str]) -> dict[str, dict]:
        return usage_store.get_usage_for_runs(run_ids) if run_ids else {}

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
            "session_state": record.session_state,
            "created_at": record.created_at.isoformat() if record.created_at else None,
            "updated_at": record.updated_at.isoformat() if record.updated_at else None,
        })

    @router.get("/sessions/{session_id}/messages")
    async def get_session_messages(session_id: str):
        if not _db_available():
            return JSONResponse(
                status_code=503,
                content={"error": "Database not configured"},
            )
        record = session_store.get(session_id)
        if record is None:
            return JSONResponse(status_code=404, content={"error": "Session not found"})
        # Return full history if available, fall back to (possibly compacted) message_history
        messages = record.full_message_history or record.message_history or []
        return JSONResponse(content={"messages": messages})

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
        child_runs = agent_run_store.list_subagent_runs(run_id)
        usage_by_run = _usage_for_runs([run_id, *[child["id"] for child in child_runs]])
        return JSONResponse(content={
            "id": record.id,
            "session_id": record.session_id,
            "parent_run_id": record.parent_run_id,
            "parent_task_id": record.parent_task_id,
            "agent_name": record.agent_name,
            "status": record.status,
            "input_query": record.input_query,
            "response": record.response,
            "message_history": record.message_history,
            "compacted_history": record.compacted_history,
            "reasoning": record.reasoning,
            "error": record.error,
            "event_count": record.event_count,
            "started_at": record.started_at.isoformat() if record.started_at else None,
            "finished_at": record.finished_at.isoformat() if record.finished_at else None,
            "usage": usage_by_run.get(run_id),
            "subagent_runs": [
                {
                    **child,
                    "usage": usage_by_run.get(child["id"]),
                }
                for child in child_runs
            ],
        })

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
