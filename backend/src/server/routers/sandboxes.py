"""Sandbox API routes."""

from __future__ import annotations

import asyncio
import logging

from fastapi import APIRouter, Query
from fastapi.responses import JSONResponse
from pydantic import BaseModel, Field

from sandbox.api import lifecycle as sb_lifecycle
from sandbox.api.raw_exec import raw_exec

logger = logging.getLogger(__name__)


class CreateSandboxRequest(BaseModel):
    name: str = Field(min_length=1)
    snapshot: str | None = None
    image: str | None = None
    env_vars: dict[str, str] = Field(default_factory=dict)
    labels: dict[str, str] = Field(default_factory=dict)


class ExecRequest(BaseModel):
    command: str
    timeout: int = 30


async def _call(func, *args, status: int = 200, **kwargs) -> JSONResponse:
    """Run a sync service call in a thread with standard error handling."""
    try:
        result = await asyncio.to_thread(func, *args, **kwargs)
        return JSONResponse(status_code=status, content=result)
    except ValueError as exc:
        return JSONResponse(status_code=404, content={"error": str(exc)})
    except Exception as exc:
        return JSONResponse(status_code=500, content={"error": str(exc)})


def create_sandbox_router() -> APIRouter:
    """Build the sandbox API router."""
    router = APIRouter(prefix="/api/sandboxes")

    # --- Static path routes MUST be registered before parameterized routes ---

    @router.get("/health")
    async def sandbox_health():
        """Check sandbox provider connection health."""
        return await _call(sb_lifecycle.get_health)

    @router.get("/available/snapshots")
    async def list_snapshots():
        """List available sandbox snapshots."""
        try:
            items = await asyncio.to_thread(sb_lifecycle.list_snapshots)
            return JSONResponse(content=items)
        except Exception as exc:
            logger.warning("Failed to list snapshots: %s", exc)
            return JSONResponse(content=[])

    @router.get("")
    async def list_sandboxes():
        """List all sandboxes."""
        return await _call(sb_lifecycle.list_sandboxes)

    # --- Parameterized routes below ---

    @router.post("")
    async def create_sandbox(req: CreateSandboxRequest):
        """Create a new sandbox."""
        return await _call(
            sb_lifecycle.create_sandbox,
            name=req.name,
            snapshot=req.snapshot,
            image=req.image,
            env_vars=req.env_vars,
            labels=req.labels,
            status=201,
        )

    @router.get("/{sandbox_id}")
    async def get_sandbox(sandbox_id: str):
        """Get a single sandbox."""
        return await _call(sb_lifecycle.get_sandbox, sandbox_id)

    @router.post("/{sandbox_id}/start")
    async def start_sandbox(sandbox_id: str):
        """Start a stopped sandbox."""
        return await _call(sb_lifecycle.start_sandbox, sandbox_id)

    @router.post("/{sandbox_id}/stop")
    async def stop_sandbox(sandbox_id: str):
        """Stop a running sandbox."""
        return await _call(sb_lifecycle.stop_sandbox, sandbox_id)

    @router.delete("/{sandbox_id}")
    async def delete_sandbox(sandbox_id: str):
        """Delete a sandbox."""
        try:
            await asyncio.to_thread(sb_lifecycle.delete_sandbox, sandbox_id)
            return JSONResponse(status_code=204, content=None)
        except Exception as exc:
            return JSONResponse(status_code=500, content={"error": str(exc)})

    @router.post("/{sandbox_id}/exec")
    async def exec_in_sandbox(sandbox_id: str, req: ExecRequest):
        """Execute a command in a sandbox."""
        try:
            await asyncio.to_thread(sb_lifecycle.ensure_sandbox_running, sandbox_id)
            resp = await raw_exec(sandbox_id, req.command, timeout=req.timeout)
            return JSONResponse(
                content={
                    "result": resp.stdout,
                    "exit_code": resp.exit_code,
                }
            )
        except Exception as exc:
            return JSONResponse(status_code=500, content={"error": str(exc)})

    @router.get("/{sandbox_id}/preview-url")
    async def get_preview_url(
        sandbox_id: str,
        port: int = Query(default=3000, ge=1, le=65535),
    ):
        """Get a preview URL for a sandbox port."""
        return await _call(sb_lifecycle.get_signed_preview_url, sandbox_id, port)

    return router
