"""Sandbox (Daytona) API routes."""

from __future__ import annotations

import logging
import os

from fastapi import APIRouter, Request
from fastapi.responses import JSONResponse
from pydantic import BaseModel, Field

logger = logging.getLogger(__name__)


class CreateSandboxRequest(BaseModel):
    name: str
    env_vars: dict[str, str] = Field(default_factory=dict)
    labels: dict[str, str] = Field(default_factory=dict)


class ExecRequest(BaseModel):
    command: str
    timeout: int = 30


def _format_state(state: object) -> str:
    """Normalize sandbox state to lowercase string."""
    return str(state).replace("SandboxState.", "").lower()


def create_sandbox_router() -> APIRouter:
    """Build the sandbox API router."""
    router = APIRouter(prefix="/api/sandboxes")

    @router.get("/health")
    async def sandbox_health():
        """Check Daytona connection health."""
        # Read from settings first, env vars as fallback
        api_key = api_url = target = ""
        try:
            from ephemeralos.config import load_settings
            settings = load_settings()
            api_key = settings.daytona_api_key.strip()
            api_url = settings.daytona_api_url.strip()
            target = settings.daytona_target.strip()
        except Exception:
            pass
        if not api_key:
            api_key = os.environ.get("DAYTONA_API_KEY", "").strip()
        if not api_url:
            api_url = os.environ.get("DAYTONA_API_URL", "").strip()
        if not target:
            target = os.environ.get("DAYTONA_TARGET", "").strip()
        if not api_key or not api_url:
            return JSONResponse(content={
                "configured": False,
                "available": False,
                "api_url": api_url or None,
                "target": target or None,
                "detail": "Set daytona_api_key/daytona_api_url in settings.json or DAYTONA_API_KEY/DAYTONA_API_URL env vars",
            })
        try:
            from ephemeralos.toolkits.integrations.daytona_toolkit.client import get_daytona_client
            client = get_daytona_client()
            result = client.list(limit=1)
            return JSONResponse(content={
                "configured": True,
                "available": True,
                "api_url": api_url,
                "target": target or None,
                "detail": f"Connected ({result.total} sandboxes)",
            })
        except Exception as exc:
            return JSONResponse(content={
                "configured": True,
                "available": False,
                "api_url": api_url,
                "target": target or None,
                "detail": str(exc),
            })

    @router.get("")
    async def list_sandboxes():
        """List all Daytona sandboxes."""
        try:
            from ephemeralos.toolkits.integrations.daytona_toolkit.client import get_daytona_client
            client = get_daytona_client()
            result = client.list()
            sandboxes = []
            for sb in result.items:
                sandboxes.append({
                    "id": sb.id,
                    "name": sb.name,
                    "state": _format_state(sb.state),
                    "labels": dict(sb.labels) if sb.labels else {},
                    "created_at": sb.created_at,
                    "cpu": sb.cpu,
                    "memory": sb.memory,
                    "disk": sb.disk,
                })
            return JSONResponse(content=sandboxes)
        except Exception as exc:
            return JSONResponse(status_code=503, content={"error": str(exc)})

    @router.post("")
    async def create_sandbox(req: CreateSandboxRequest):
        """Create a new Daytona sandbox."""
        try:
            from ephemeralos.toolkits.integrations.daytona_toolkit.client import get_daytona_client
            from daytona_sdk import CreateSandboxFromImageParams
            client = get_daytona_client()
            params = CreateSandboxFromImageParams(
                name=req.name,
                language="python",
                env_vars=req.env_vars or None,
                labels=req.labels or None,
            )
            sandbox = client.create(params, timeout=120)
            return JSONResponse(status_code=201, content={
                "id": sandbox.id,
                "name": sandbox.name,
                "state": _format_state(sandbox.state),
            })
        except Exception as exc:
            return JSONResponse(status_code=500, content={"error": str(exc)})

    @router.post("/{sandbox_id}/start")
    async def start_sandbox(sandbox_id: str):
        """Start a stopped sandbox."""
        try:
            from ephemeralos.toolkits.integrations.daytona_toolkit.client import get_daytona_client
            client = get_daytona_client()
            sandbox = client.get(sandbox_id)
            client.start(sandbox, timeout=60)
            sandbox.refresh_data()
            return JSONResponse(content={
                "id": sandbox.id,
                "state": _format_state(sandbox.state),
            })
        except Exception as exc:
            return JSONResponse(status_code=500, content={"error": str(exc)})

    @router.post("/{sandbox_id}/stop")
    async def stop_sandbox(sandbox_id: str):
        """Stop a running sandbox."""
        try:
            from ephemeralos.toolkits.integrations.daytona_toolkit.client import get_daytona_client
            client = get_daytona_client()
            sandbox = client.get(sandbox_id)
            client.stop(sandbox, timeout=60)
            sandbox.refresh_data()
            return JSONResponse(content={
                "id": sandbox.id,
                "state": _format_state(sandbox.state),
            })
        except Exception as exc:
            return JSONResponse(status_code=500, content={"error": str(exc)})

    @router.delete("/{sandbox_id}")
    async def delete_sandbox(sandbox_id: str):
        """Delete a sandbox."""
        try:
            from ephemeralos.toolkits.integrations.daytona_toolkit.client import get_daytona_client
            client = get_daytona_client()
            sandbox = client.get(sandbox_id)
            client.delete(sandbox)
            return JSONResponse(status_code=204, content=None)
        except Exception as exc:
            return JSONResponse(status_code=500, content={"error": str(exc)})

    @router.post("/{sandbox_id}/exec")
    async def exec_in_sandbox(sandbox_id: str, req: ExecRequest):
        """Execute a command in a sandbox."""
        try:
            from ephemeralos.toolkits.integrations.daytona_toolkit.client import get_sandbox
            sandbox = get_sandbox(sandbox_id)
            resp = sandbox.process.exec(req.command, timeout=req.timeout)
            return JSONResponse(content={
                "result": resp.result,
                "exit_code": resp.exit_code,
            })
        except Exception as exc:
            return JSONResponse(status_code=500, content={"error": str(exc)})

    @router.get("/{sandbox_id}/files")
    async def list_sandbox_files(sandbox_id: str, path: str = "/home/daytona"):
        """List files in a sandbox directory."""
        try:
            from ephemeralos.toolkits.integrations.daytona_toolkit.client import get_sandbox
            sandbox = get_sandbox(sandbox_id)
            entries = sandbox.fs.list_files(path)
            files = [
                {
                    "name": e.name,
                    "is_dir": e.is_dir,
                    "size": e.size,
                    "path": f"{path.rstrip('/')}/{e.name}",
                }
                for e in entries
            ]
            return JSONResponse(content=files)
        except Exception as exc:
            return JSONResponse(status_code=500, content={"error": str(exc)})

    return router
