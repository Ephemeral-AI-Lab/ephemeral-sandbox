"""Agent definition CRUD API router."""

from __future__ import annotations

import logging
from typing import TYPE_CHECKING, Any, Callable

from fastapi import APIRouter, HTTPException, Query
from fastapi.responses import JSONResponse

from ephemeralos.agents.registry import get_definition, list_definitions
from ephemeralos.agents.api.schemas import (
    AgentDefinitionCreate,
    AgentDefinitionResponse,
    AgentDefinitionUpdate,
    AgentValidationResult,
    CloneRequest,
)

if TYPE_CHECKING:
    from ephemeralos.agents.builder.service import AgentBuilderService
    from ephemeralos.tools.base import ToolRegistry

logger = logging.getLogger(__name__)


def create_agents_router(
    get_builder_service: Callable[[], "AgentBuilderService | None"],
    get_tool_registry: Callable[[], "ToolRegistry | None"],
) -> APIRouter:
    router = APIRouter(prefix="/api/agents", tags=["agents"])

    def _require_builder() -> "AgentBuilderService":
        svc = get_builder_service()
        if svc is None:
            raise HTTPException(status_code=503, detail="Agent builder not available (database not configured)")
        return svc

    @router.get("")
    @router.get("/")
    async def list_agents(
        source: str | None = Query(default=None),
        tags: str | None = Query(default=None),
    ) -> list[dict[str, Any]]:
        defs = list_definitions(source=source)
        return [
            {"name": d.name, "description": d.description, "source": d.source,
             "model": d.model, "subagent_type": d.subagent_type,
             "background": d.background}
            for d in defs
        ]

    @router.get("/toolkits/available")
    async def list_available_toolkits() -> list[str]:
        from ephemeralos.tools.factory import list_factories  # noqa: PLC0415
        names: set[str] = set()
        tr = get_tool_registry()
        if tr:
            names.update(tk.name for tk in tr.list_toolkits())
        names.update(list_factories())
        return sorted(names)

    @router.get("/{name}")
    async def get_agent(name: str) -> dict[str, Any]:
        defn = get_definition(name)
        if defn is None:
            raise HTTPException(status_code=404, detail=f"Agent '{name}' not found")
        return defn.model_dump()

    @router.post("/", status_code=201, response_model=AgentDefinitionResponse)
    async def create_agent(body: AgentDefinitionCreate) -> AgentDefinitionResponse:
        svc = _require_builder()
        try:
            return svc.create_agent(body)
        except ValueError as exc:
            raise HTTPException(status_code=400, detail=str(exc)) from exc

    @router.put("/{name}", response_model=AgentDefinitionResponse)
    async def update_agent(name: str, body: AgentDefinitionUpdate) -> AgentDefinitionResponse:
        svc = _require_builder()
        try:
            return svc.update_agent(name, body)
        except KeyError as exc:
            raise HTTPException(status_code=404, detail=str(exc)) from exc
        except ValueError as exc:
            raise HTTPException(status_code=400, detail=str(exc)) from exc

    @router.delete("/{name}")
    async def delete_agent(name: str) -> JSONResponse:
        svc = _require_builder()
        ok = svc.delete_agent(name)
        if not ok:
            raise HTTPException(status_code=404, detail=f"Agent '{name}' not found")
        return JSONResponse(content={"deleted": name})

    @router.post("/{name}/clone", status_code=201, response_model=AgentDefinitionResponse)
    async def clone_agent(name: str, body: CloneRequest) -> AgentDefinitionResponse:
        svc = _require_builder()
        try:
            return svc.clone_agent(name, body.new_name)
        except KeyError as exc:
            raise HTTPException(status_code=404, detail=str(exc)) from exc

    @router.post("/validate", response_model=AgentValidationResult)
    async def validate_agent(body: AgentDefinitionCreate) -> AgentValidationResult:
        svc = _require_builder()
        return svc.validate_agent(body)

    return router
