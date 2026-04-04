"""Agent definition CRUD API router."""

from __future__ import annotations

import logging
from typing import TYPE_CHECKING, Any, Callable

from fastapi import APIRouter, HTTPException, Query
from fastapi.responses import JSONResponse

from ephemeralos.coordinator.agent_definitions import (
    get_definition,
    list_definitions,
)
from ephemeralos.toolkits.factory import list_factories
from ephemeralos.ui.schemas.agent_schemas import (
    AgentDefinitionCreate,
    AgentDefinitionResponse,
    AgentDefinitionUpdate,
    AgentValidationResult,
    CloneRequest,
)

if TYPE_CHECKING:
    from ephemeralos.services.agent_builder.builder import AgentBuilderService
    from ephemeralos.tools.base import ToolRegistry

logger = logging.getLogger(__name__)


def create_agents_router(
    get_builder_service: "Callable[[], AgentBuilderService | None]",
    get_tool_registry: "Callable[[], ToolRegistry | None]",
) -> APIRouter:
    """Create the /api/agents router.

    Uses getter callables so the router picks up services initialized after
    app creation (during lifespan startup).
    """
    router = APIRouter(prefix="/api/agents", tags=["agents"])

    def _require_builder() -> "AgentBuilderService":
        svc = get_builder_service()
        if svc is None:
            raise HTTPException(
                status_code=503,
                detail="Agent builder not available (database not configured)",
            )
        return svc

    # -- read endpoints (always available) -------------------------------------

    @router.get("/")
    async def list_agents(
        source: str | None = Query(default=None, description="Filter by source: builtin, user"),
        tags: str | None = Query(default=None, description="Comma-separated tags to filter by"),
    ) -> list[dict[str, Any]]:
        """List all registered agent definitions."""
        defs = list_definitions(source=source)
        if tags:
            tag_list = [t.strip() for t in tags.split(",") if t.strip()]
            if tag_list:
                # For DB agents only — built-in agents don't have tags
                defs = [d for d in defs]  # keep all for now; tag filtering is DB-side
        return [
            {
                "name": d.name,
                "description": d.description,
                "source": d.source,
                "model": d.model,
                "color": d.color,
                "subagent_type": d.subagent_type,
                "background": d.background,
            }
            for d in defs
        ]

    @router.get("/tools/available")
    async def list_available_tools() -> list[dict[str, str]]:
        """List all registered tool names."""
        tr = get_tool_registry()
        if tr is None:
            return []
        return [
            {"name": t.name, "description": t.description}
            for t in tr.list_tools()
        ]

    @router.get("/toolkits/available")
    async def list_available_toolkits() -> list[str]:
        """List all registered toolkit factory names."""
        return list_factories()

    @router.get("/{name}")
    async def get_agent(name: str) -> dict[str, Any]:
        """Get a single agent definition by name."""
        defn = get_definition(name)
        if defn is None:
            raise HTTPException(status_code=404, detail=f"Agent '{name}' not found")
        return defn.model_dump()

    # -- write endpoints (require DB) ------------------------------------------

    @router.post("/", status_code=201, response_model=AgentDefinitionResponse)
    async def create_agent(body: AgentDefinitionCreate) -> AgentDefinitionResponse:
        """Create a new agent definition (stored in DB)."""
        svc = _require_builder()
        try:
            return svc.create_agent(body)
        except ValueError as exc:
            raise HTTPException(status_code=400, detail=str(exc)) from exc

    @router.put("/{name}", response_model=AgentDefinitionResponse)
    async def update_agent(name: str, body: AgentDefinitionUpdate) -> AgentDefinitionResponse:
        """Update an existing user-created agent definition."""
        svc = _require_builder()
        try:
            return svc.update_agent(name, body)
        except KeyError as exc:
            raise HTTPException(status_code=404, detail=str(exc)) from exc
        except ValueError as exc:
            raise HTTPException(status_code=400, detail=str(exc)) from exc

    @router.delete("/{name}")
    async def delete_agent(name: str) -> JSONResponse:
        """Soft-delete a user-created agent definition."""
        svc = _require_builder()
        ok = svc.delete_agent(name)
        if not ok:
            raise HTTPException(status_code=404, detail=f"Agent '{name}' not found")
        return JSONResponse(content={"deleted": name})

    @router.post("/{name}/clone", status_code=201, response_model=AgentDefinitionResponse)
    async def clone_agent(name: str, body: CloneRequest) -> AgentDefinitionResponse:
        """Clone an agent definition under a new name."""
        svc = _require_builder()
        try:
            return svc.clone_agent(name, body.new_name)
        except KeyError as exc:
            raise HTTPException(status_code=404, detail=str(exc)) from exc

    @router.post("/validate", response_model=AgentValidationResult)
    async def validate_agent(body: AgentDefinitionCreate) -> AgentValidationResult:
        """Dry-run validation without persisting."""
        svc = _require_builder()
        return svc.validate_agent(body)

    return router
