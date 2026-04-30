"""Persistence API routes for TaskCenter requests, graphs, and agent runs."""

from __future__ import annotations

from db.stores import AgentRunStore, TaskCenterStore

from fastapi import APIRouter
from fastapi.responses import JSONResponse


def create_persistence_router(
    task_center_store: TaskCenterStore,
    agent_run_store: AgentRunStore,
) -> APIRouter:
    """Build the persistence API router."""
    router = APIRouter(prefix="/api/db")

    def _db_available() -> bool:
        return task_center_store._session_factory is not None

    @router.get("/task-center-requests")
    async def list_task_center_requests(cwd: str | None = None, limit: int = 20):
        if not _db_available():
            return JSONResponse(
                status_code=503,
                content={"error": "Database not configured"},
            )
        requests = task_center_store.list_requests(cwd=cwd, limit=limit)
        return JSONResponse(content={"task_center_requests": requests})

    @router.get("/task-center-requests/{request_id}")
    async def get_task_center_request(request_id: str):
        if not _db_available():
            return JSONResponse(
                status_code=503,
                content={"error": "Database not configured"},
            )
        record = task_center_store.get_request(request_id)
        if record is None:
            return JSONResponse(status_code=404, content={"error": "Request not found"})
        return JSONResponse(
            content={
                "id": record.id,
                "cwd": record.cwd,
                "sandbox_id": record.sandbox_id,
                "request_prompt": record.request_prompt,
                "created_at": record.created_at.isoformat() if record.created_at else None,
                "updated_at": record.updated_at.isoformat() if record.updated_at else None,
            }
        )

    @router.get("/task-center-requests/{request_id}/runs")
    async def list_task_center_runs(request_id: str, limit: int = 50):
        if not _db_available():
            return JSONResponse(
                status_code=503,
                content={"error": "Database not configured"},
            )
        runs = task_center_store.list_runs_for_request(request_id, limit=limit)
        return JSONResponse(content={"runs": runs})

    @router.get("/task-center-runs/{task_center_run_id}/tasks")
    async def list_task_center_tasks(task_center_run_id: str):
        if not _db_available():
            return JSONResponse(
                status_code=503,
                content={"error": "Database not configured"},
            )
        tasks = task_center_store.list_tasks_for_run(task_center_run_id)
        agent_runs = agent_run_store.list_runs_for_tasks([task["id"] for task in tasks])
        runs_by_task_id = {run["task_id"]: run for run in agent_runs}
        return JSONResponse(
            content={
                "tasks": [
                    {
                        **task,
                        "agent_run": runs_by_task_id.get(task["id"]),
                    }
                    for task in tasks
                ]
            }
        )

    @router.get("/task-center-runs/{task_center_run_id}/graph")
    async def list_task_center_harness_graph(task_center_run_id: str):
        if not _db_available():
            return JSONResponse(
                status_code=503,
                content={"error": "Database not configured"},
            )
        harness_graphs = task_center_store.list_harness_graphs_for_run(
            task_center_run_id
        )
        return JSONResponse(content={"harness_graphs": harness_graphs})

    @router.get("/agent-runs/{agent_run_id}")
    async def get_agent_run(agent_run_id: str):
        if not _db_available():
            return JSONResponse(
                status_code=503,
                content={"error": "Database not configured"},
            )
        record = agent_run_store.get_run(agent_run_id)
        if record is None:
            return JSONResponse(status_code=404, content={"error": "Agent run not found"})
        return JSONResponse(
            content={
                "id": record.id,
                "task_id": record.task_id,
                "agent_name": record.agent_name,
                "message_history": record.message_history,
                "terminal_tool_result": record.terminal_tool_result,
                "token_count": record.token_count,
                "error": record.error,
                "created_at": record.created_at.isoformat() if record.created_at else None,
                "finished_at": record.finished_at.isoformat() if record.finished_at else None,
            }
        )

    @router.get("/health")
    async def db_health():
        return JSONResponse(
            content={
                "database": "connected" if _db_available() else "not_configured",
            }
        )

    return router
